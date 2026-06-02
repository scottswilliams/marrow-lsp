//! Spawn and own the run-thread.
//!
//! The interpreter is single-threaded, so the whole run — program, store, host,
//! and the [`Debugger`] hook — lives on one dedicated thread. The protocol thread
//! holds only channel ends and a join handle. The store moves onto the run-thread
//! (it is `Send`); nothing that borrows the interpreter env ever leaves it.

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::{self, JoinHandle};

use marrow_run::{Host, RuntimeError, Value, run_entry_with_debugger};

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

/// Start the run on a dedicated thread: prepare the launch (program, entry,
/// store), build the [`Debugger`] with prototype breakpoint lines, and run.
/// Returns the handle and channels, or an error string if the launch could not be
/// prepared. Preparing here (again) keeps the store open on the thread that uses
/// it, never crossing back to the protocol thread.
pub fn spawn(
    project_dir: std::path::PathBuf,
    entry: Option<String>,
    args: Vec<Value>,
    breakpoint_lines: Vec<u32>,
    stop_on_entry: bool,
) -> Result<RunHandle, String> {
    let launch = crate::project::prepare(&project_dir, entry.as_deref())
        .map_err(|error| error.to_string())?;

    let (event_tx, event_rx) = channel::<RunEvent>();
    let (control_tx, control_rx) = channel::<Control>();

    let handle = thread::Builder::new()
        .name("marrow-dap-run".to_string())
        .spawn(move || {
            run_on_thread(
                launch,
                args,
                breakpoint_lines,
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
    launch: crate::project::Launch,
    args: Vec<Value>,
    breakpoint_lines: Vec<u32>,
    stop_on_entry: bool,
    events: Sender<RunEvent>,
    control: Receiver<Control>,
) -> Option<Outcome> {
    let crate::project::Launch {
        program,
        entry,
        store,
    } = launch;
    let debugger = Debugger::new(
        breakpoint_lines,
        stop_on_entry,
        program.clone(),
        events,
        control,
    );
    // Dispatch on the concrete store so its `&RefCell<S>` unsizes to the
    // `&RefCell<dyn Backend>` the runtime takes — a trait object behind a `Box`
    // could not sit directly in a `RefCell<dyn Backend>`.
    match store {
        crate::project::LaunchStore::Memory(store) => {
            run_with_store(store, &program, &entry, &args, debugger)
        }
        crate::project::LaunchStore::Native(store) => {
            run_with_store(*store, &program, &entry, &args, debugger)
        }
    }
}

/// Run the entry over a concrete store `S`, threading the debugger. The store sits
/// in a stack `RefCell<S>` whose `&` unsizes to `&RefCell<dyn Backend>` at the call.
fn run_with_store<S: marrow_store::backend::Backend + 'static>(
    store: S,
    program: &marrow_check::CheckedProgram,
    entry: &str,
    args: &[Value],
    mut debugger: Debugger,
) -> Option<Outcome> {
    let store = RefCell::new(store);
    // A debug run gets a real clock and a captured log sink, so `std::clock`/
    // `std::log` work; it gets no filesystem and no maintenance.
    let log = std::rc::Rc::new(RefCell::new(String::new()));
    let host = Host::new()
        .with_system_clock()
        .with_log_sink(std::rc::Rc::clone(&log));
    let result = run_entry_with_debugger(program, &store, &host, &mut debugger, entry, args);
    Some(outcome(result, &log.borrow()))
}

/// Build the [`Outcome`] from the run's result and the captured log. A debugger
/// terminate is a clean stop, not a fault. The run's `print`/`write` output is in
/// `RunOutput::output`; the log sink carries `std::log` lines, appended after.
fn outcome(result: Result<marrow_run::RunOutput, RuntimeError>, log: &str) -> Outcome {
    match result {
        Ok(run) => {
            let mut output = run.output;
            if !log.is_empty() {
                output.push_str(log);
            }
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
        Err(error) if error.code == crate::debugger::RUN_TERMINATED => Outcome {
            output: log.to_string(),
            result: Ok(String::new()),
        },
        Err(error) => Outcome {
            output: log.to_string(),
            result: Err(format!("{}: {}", error.code, error.message)),
        },
    }
}
