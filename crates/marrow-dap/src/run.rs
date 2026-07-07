//! Spawn and own the run-thread.
//!
//! The interpreter is single-threaded, so the whole run — project session, host,
//! and the [`Debugger`] hook — lives on one dedicated thread. The protocol thread
//! holds only channel ends and a join handle. Nothing that borrows the interpreter
//! env ever leaves the run-thread.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};

use marrow_check::AnalysisGeneration;
use marrow_lsp_core::execution::{execution_boundary_json, surface_serve_boundary_json};
use marrow_run::{
    DebugValue, EntryInvocation, Host, ProjectInvokeError, ProjectSession, RunOutput, SessionEntry,
    SourceAnalysisAdmission, SystemNondeterminism,
};
use serde_json::Value as Json;

use crate::debugger::{ArmedBreakpoints, Control, Debugger, RunEvent};

/// The result of a finished run, sent back through the join handle: anything the
/// run printed plus its outcome message (the returned value rendered, or a fault
/// message — except the debugger's own terminate, which is a clean stop).
pub struct Outcome {
    pub output: String,
    /// `Ok` with the rendered return value (possibly empty) on a clean finish or a
    /// debugger terminate; `Err` with a fault message on a real runtime fault.
    pub result: Result<String, String>,
}

/// A live run-thread: its join handle (yielding the [`Outcome`]) and the channels
/// that drive and observe it.
pub struct RunHandle {
    pub handle: JoinHandle<Option<Outcome>>,
    pub control: Sender<Control>,
    pub events: Receiver<RunEvent>,
    pub terminate: Arc<AtomicBool>,
    pub pause: Arc<AtomicBool>,
    pub breakpoints: Sender<ArmedBreakpoints>,
    pub execution_boundary: Json,
    pub surface_serve_boundary: Json,
}

#[derive(Debug)]
pub enum SpawnError {
    Launch(crate::project::LaunchError),
    Thread(String),
}

struct RunRequest {
    project_dir: std::path::PathBuf,
    entry: Option<String>,
    analysis_generation: AnalysisGeneration,
    source_analysis_admission: Option<SourceAnalysisAdmission>,
    invocation: EntryInvocation,
    stop_on_entry: bool,
    breakpoints: ArmedBreakpoints,
    terminate: Arc<AtomicBool>,
    pause: Arc<AtomicBool>,
    breakpoint_swaps: Receiver<ArmedBreakpoints>,
}

struct RunStart {
    execution_boundary: Json,
    surface_serve_boundary: Json,
}

/// Start the run on a dedicated thread.
pub fn spawn(
    project_dir: std::path::PathBuf,
    entry: Option<String>,
    analysis_generation: AnalysisGeneration,
    source_analysis_admission: Option<SourceAnalysisAdmission>,
    invocation: EntryInvocation,
    stop_on_entry: bool,
    breakpoints: ArmedBreakpoints,
) -> Result<RunHandle, SpawnError> {
    let (event_tx, event_rx) = channel::<RunEvent>();
    let (control_tx, control_rx) = channel::<Control>();
    let (breakpoint_tx, breakpoint_rx) = channel::<ArmedBreakpoints>();
    let (start_tx, start_rx) = channel::<Result<RunStart, crate::project::LaunchError>>();
    let terminate = Arc::new(AtomicBool::new(false));
    let pause = Arc::new(AtomicBool::new(false));
    let request = RunRequest {
        project_dir,
        entry,
        analysis_generation,
        source_analysis_admission,
        invocation,
        stop_on_entry,
        breakpoints,
        terminate: Arc::clone(&terminate),
        pause: Arc::clone(&pause),
        breakpoint_swaps: breakpoint_rx,
    };

    let handle = thread::Builder::new()
        .name("marrow-dap-run".to_string())
        .spawn(move || run_on_thread(request, event_tx, control_rx, start_tx))
        .map_err(|error| SpawnError::Thread(format!("could not start the run thread: {error}")))?;

    let start = match start_rx.recv() {
        Ok(Ok(start)) => start,
        Ok(Err(error)) => {
            let _ = handle.join();
            return Err(SpawnError::Launch(error));
        }
        Err(_) => {
            let _ = handle.join();
            return Err(SpawnError::Thread(
                "run thread stopped before launch admission".to_string(),
            ));
        }
    };

    Ok(RunHandle {
        handle,
        control: control_tx,
        events: event_rx,
        terminate,
        pause,
        breakpoints: breakpoint_tx,
        execution_boundary: start.execution_boundary,
        surface_serve_boundary: start.surface_serve_boundary,
    })
}

/// The run-thread body: own the session for the whole run, install the
/// [`Debugger`], and run the entry. The live frame the hook serves never escapes
/// this thread.
/// Returns the [`Outcome`].
fn run_on_thread(
    request: RunRequest,
    events: Sender<RunEvent>,
    control: Receiver<Control>,
    start: Sender<Result<RunStart, crate::project::LaunchError>>,
) -> Option<Outcome> {
    let RunRequest {
        project_dir,
        entry,
        analysis_generation,
        source_analysis_admission,
        invocation,
        stop_on_entry,
        breakpoints,
        terminate,
        pause,
        breakpoint_swaps,
    } = request;
    let launch = match crate::project::prepare_admitted(
        &project_dir,
        entry.as_deref(),
        &analysis_generation,
        source_analysis_admission.as_ref(),
        invocation,
    ) {
        Ok(launch) => launch,
        Err(error) => {
            let message = error.to_string();
            let _ = start.send(Err(error));
            return Some(Outcome {
                output: String::new(),
                result: Err(message),
            });
        }
    };
    let surface_serve_boundary = match launch.surface_serve_boundary() {
        Ok(boundary) => surface_serve_boundary_json(&boundary),
        Err(error) => {
            let message = error.to_string();
            let _ = start.send(Err(error));
            return Some(Outcome {
                output: String::new(),
                result: Err(message),
            });
        }
    };
    let execution_boundary = execution_boundary_json(&launch.session);
    let crate::project::Launch {
        session,
        invocation,
        ..
    } = launch;
    if start
        .send(Ok(RunStart {
            execution_boundary,
            surface_serve_boundary,
        }))
        .is_err()
    {
        return None;
    }
    let debugger = Debugger::new(
        stop_on_entry,
        breakpoints,
        events,
        control,
        terminate,
        pause,
        breakpoint_swaps,
    );
    run_with_session(&session, invocation, debugger)
}

/// Run the entry through the project session, threading the debugger.
fn run_with_session(
    session: &ProjectSession,
    invocation: EntryInvocation,
    mut debugger: Debugger,
) -> Option<Outcome> {
    // A debug run gets a real clock and a captured log sink, so `std::clock`/
    // `std::log` work; it gets no filesystem and no maintenance.
    let log = std::rc::Rc::new(RefCell::new(String::new()));
    let host = Host::new()
        .with_nondeterminism(&SystemNondeterminism::new())
        .with_log_sink(std::rc::Rc::clone(&log));
    let mut output = String::new();
    let result = session.invoke(
        SessionEntry::protocol(invocation, &host, &mut output)
            .with_hook(&mut debugger)
            .with_isolated_writes(),
    );
    Some(outcome(result, output, &log.borrow()))
}

/// Build the [`Outcome`] from the run's result and the captured log. A debugger
/// terminate is a clean stop, not a fault. The run's `print`/`write` output is in
/// `output`; the log sink carries `std::log` lines, appended after.
fn outcome(
    result: Result<RunOutput, ProjectInvokeError>,
    mut output: String,
    log: &str,
) -> Outcome {
    if !log.is_empty() {
        output.push_str(log);
    }
    match result {
        Ok(run) => {
            let rendered = run
                .value
                .map(|value| DebugValue::from_value(value).preview())
                .unwrap_or_default();
            Outcome {
                output,
                result: Ok(rendered),
            }
        }
        // The debugger's own terminate fault is a clean stop, not a program error.
        Err(ProjectInvokeError::Runtime(error))
            if error.code() == crate::debugger::RUN_TERMINATED =>
        {
            Outcome {
                output,
                result: Ok(String::new()),
            }
        }
        Err(error) => Outcome {
            output,
            result: Err(format!("{}: {}", error.code(), error.message())),
        },
    }
}

#[cfg(test)]
mod tests {
    use marrow_run::RuntimeError;
    use marrow_syntax::SourceSpan;

    use super::*;

    fn runtime_error(code: &'static str) -> RuntimeError {
        RuntimeError::fatal(code, "boom", SourceSpan::default())
    }

    #[test]
    fn outcome_preserves_output_and_logs_on_runtime_fault() {
        let outcome = outcome(
            Err(ProjectInvokeError::Runtime(runtime_error("run.assertion"))),
            "before fault\n".to_string(),
            "log line\n",
        );

        assert_eq!(outcome.output, "before fault\nlog line\n");
        assert!(outcome.result.unwrap_err().contains("run.assertion"));
    }

    #[test]
    fn outcome_preserves_output_and_logs_on_debugger_termination() {
        let outcome = outcome(
            Err(ProjectInvokeError::Runtime(runtime_error(
                crate::debugger::RUN_TERMINATED,
            ))),
            "before terminate\n".to_string(),
            "log line\n",
        );

        assert_eq!(outcome.output, "before terminate\nlog line\n");
        assert_eq!(outcome.result, Ok(String::new()));
    }
}
