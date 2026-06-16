//! Spawn and own the run-thread.
//!
//! The interpreter is single-threaded, so the whole run — program, store, host,
//! and the [`Debugger`] hook — lives on one dedicated thread. The protocol thread
//! holds only channel ends and a join handle. The store moves onto the run-thread
//! (it is `Send`); nothing that borrows the interpreter env ever leaves it.

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};

use marrow_run::{
    CheckedEntryCall, Host, RuntimeError, SystemNondeterminism, Value, run_entry_with_debugger,
};
use marrow_store::tree::TreeStore;

use crate::debugger::{Control, Debugger, RunEvent};

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
}

/// Start the run on a dedicated thread. Project preparation creates the in-memory
/// store on that same thread, so store internals never cross the protocol/run
/// boundary.
pub fn spawn(
    project_dir: std::path::PathBuf,
    entry: Option<String>,
    args: Vec<Value>,
    stop_on_entry: bool,
) -> Result<RunHandle, String> {
    let (event_tx, event_rx) = channel::<RunEvent>();
    let (control_tx, control_rx) = channel::<Control>();

    let handle = thread::Builder::new()
        .name("marrow-dap-run".to_string())
        .spawn(move || {
            run_on_thread(
                project_dir,
                entry,
                args,
                stop_on_entry,
                event_tx,
                control_rx,
            )
        })
        .map_err(|error| format!("could not start the run thread: {error}"))?;

    Ok(RunHandle {
        handle,
        control: control_tx,
        events: event_rx,
    })
}

/// The run-thread body: own the store for the whole run, install the [`Debugger`],
/// and run the entry. The live frame the hook serves never escapes this thread.
/// Returns the [`Outcome`].
fn run_on_thread(
    project_dir: std::path::PathBuf,
    requested_entry: Option<String>,
    args: Vec<Value>,
    stop_on_entry: bool,
    events: Sender<RunEvent>,
    control: Receiver<Control>,
) -> Option<Outcome> {
    let launch = match crate::project::prepare(&project_dir, requested_entry.as_deref()) {
        Ok(launch) => launch,
        Err(error) => {
            return Some(Outcome {
                output: String::new(),
                result: Err(error.to_string()),
            });
        }
    };
    let crate::project::Launch {
        program,
        entry,
        store,
    } = launch;
    let debugger = Debugger::new(stop_on_entry, events, control);
    run_with_store(store, &program, &entry, &args, debugger)
}

/// Run the entry over the launch store, threading the debugger.
fn run_with_store(
    store: TreeStore,
    program: &marrow_check::CheckedProgram,
    entry: &str,
    args: &[Value],
    mut debugger: Debugger,
) -> Option<Outcome> {
    // A debug run gets a real clock and a captured log sink, so `std::clock`/
    // `std::log` work; it gets no filesystem and no maintenance.
    let log = std::rc::Rc::new(RefCell::new(String::new()));
    let host = Host::new()
        .with_nondeterminism(&SystemNondeterminism::new())
        .with_log_sink(std::rc::Rc::clone(&log));
    let runtime = program.runtime();
    let mut output = String::new();
    let result = CheckedEntryCall::new(&runtime, entry, args.to_vec())
        .and_then(|call| run_entry_with_debugger(&store, &host, &mut debugger, &call, &mut output));
    Some(outcome(result, output, &log.borrow()))
}

/// Build the [`Outcome`] from the run's result and the captured log. A debugger
/// terminate is a clean stop, not a fault. The run's `print`/`write` output is in
/// `output`; the log sink carries `std::log` lines, appended after.
fn outcome(
    result: Result<marrow_run::RunOutput, RuntimeError>,
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
                .map(|value| value.display_debug())
                .unwrap_or_default();
            Outcome {
                output,
                result: Ok(rendered),
            }
        }
        // The debugger's own terminate fault is a clean stop, not a program error.
        Err(error) if error.code() == crate::debugger::RUN_TERMINATED => Outcome {
            output,
            result: Ok(String::new()),
        },
        Err(error) => Outcome {
            output,
            result: Err(format!("{}: {}", error.code(), error.message)),
        },
    }
}

#[cfg(test)]
mod tests {
    use marrow_syntax::SourceSpan;

    use super::*;

    fn runtime_error(code: &'static str) -> RuntimeError {
        RuntimeError::fatal(code, "boom", SourceSpan::default())
    }

    #[test]
    fn outcome_preserves_output_and_logs_on_runtime_fault() {
        let outcome = outcome(
            Err(runtime_error("run.assertion")),
            "before fault\n".to_string(),
            "log line\n",
        );

        assert_eq!(outcome.output, "before fault\nlog line\n");
        assert!(outcome.result.unwrap_err().contains("run.assertion"));
    }

    #[test]
    fn outcome_preserves_output_and_logs_on_debugger_termination() {
        let outcome = outcome(
            Err(runtime_error(crate::debugger::RUN_TERMINATED)),
            "before terminate\n".to_string(),
            "log line\n",
        );

        assert_eq!(outcome.output, "before terminate\nlog line\n");
        assert_eq!(outcome.result, Ok(String::new()));
    }
}
