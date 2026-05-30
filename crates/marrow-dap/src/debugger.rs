//! The run-thread side of the debugger: the [`StepHook`] that parks the run at a
//! stop and serves the protocol thread's inspection queries without ever letting
//! the live [`Frame`] cross a thread boundary.
//!
//! ## The rendezvous (no `unsafe`, no cross-thread `Env` borrow)
//! The interpreter is single-threaded and synchronous, so the run owns its store
//! and locals for as long as it executes. We run the run on a dedicated
//! *run-thread* via [`run_entry_with_debugger`]. [`StepHook::before_statement`] is
//! the rendezvous point: it records the current span and depth, asks the shared
//! [`step`](crate::step) policy whether this statement is a stop, and if so:
//!
//! 1. sends a [`RunEvent::Stopped`] to the *protocol thread* over a channel, then
//! 2. blocks in a serve-loop on its control channel.
//!
//! While parked *inside* `before_statement`, the run-thread still holds the
//! `Frame` borrow, so it — and only it — can read locals and the live store. The
//! protocol thread never touches the frame; it sends [`Control::Query`] messages
//! and reads back owned [`QueryResult`] values the run-thread computed. A
//! [`Control::Resume`] sets the next step mode and returns `Ok(())` so the run
//! proceeds; a [`Control::Terminate`] returns `Err`, which the runtime unwinds —
//! rolling back any open transaction. The frame borrow is thus confined to one
//! thread by construction; the channels carry only `Send` owned data.

use std::sync::mpsc::{Receiver, Sender};

use marrow_check::CheckedProgram;
use marrow_run::{Frame, RuntimeError, StepHook, Value};
use marrow_store::path::PathSegment;

use crate::durable::{self, DurableNode};
use crate::step::{self, Resume, StopReason};
use crate::variables::{self, Child};

/// The `run.terminated` fault code the hook returns to unwind the run on a client
/// `terminate`/`disconnect`. It surfaces like any runtime fault, so an open
/// `transaction` rolls back as the run stack unwinds.
pub const RUN_TERMINATED: &str = "run.terminated";

/// A snapshot of the locals visible at a stop: each name with its one-line
/// rendering and, for a compound value, the owned value to expand. Captured on
/// the run-thread by cloning out of the frame, so it is `Send` and the frame
/// borrow stays put.
#[derive(Debug, Clone)]
pub struct LocalsSnapshot {
    pub entries: Vec<LocalVar>,
}

/// One local at a stop.
#[derive(Debug, Clone)]
pub struct LocalVar {
    pub name: String,
    pub value: String,
    /// The owned value to expand, present only for a compound shape.
    pub expand: Option<Value>,
}

/// Where the run stopped, sent to the protocol thread so it can raise a DAP
/// `stopped` event and answer a `stackTrace` from cached data.
#[derive(Debug, Clone)]
pub struct StopInfo {
    pub reason: StopReason,
    pub line: u32,
    pub column: u32,
    pub depth: usize,
}

/// A question the protocol thread asks the parked run-thread. Each is answered
/// from the live frame and store and returns owned, `Send` data.
#[derive(Debug)]
pub enum Query {
    /// The locals visible at the current stop.
    Locals,
    /// Expand a previously-returned compound local value.
    ExpandLocal(Value),
    /// The top-level durable roots (`^...`).
    DurableRoots,
    /// The children of a durable path.
    DurableChildren(Vec<PathSegment>),
    /// Evaluate a watch/REPL expression (a `^` path or a bare local name).
    Evaluate(String),
}

/// The answer to a [`Query`], owned and `Send`.
#[derive(Debug)]
pub enum QueryResult {
    Locals(LocalsSnapshot),
    Children(Vec<Child>),
    Durable(Vec<DurableNode>),
    /// A watch/REPL result: the rendered value, or an error message for an
    /// unwatchable expression.
    Evaluated(Result<String, String>),
}

/// A control message from the protocol thread to the parked run-thread.
#[derive(Debug)]
pub enum Control {
    /// Answer a query and keep serving (the run stays parked).
    Query(Query),
    /// Resume the run with the given step mode.
    Resume(Resume),
    /// Abort the run (terminate/disconnect): the hook returns `Err`.
    Terminate,
}

/// An event from the run-thread to the protocol thread.
#[derive(Debug)]
pub enum RunEvent {
    /// The run parked at a stop; the protocol thread should emit `stopped`.
    Stopped(StopInfo),
    /// The answer to the most recent [`Query`].
    Answer(QueryResult),
}

/// The hook installed on the run-thread. It owns the channel ends to the protocol
/// thread, the breakpoint line set, the program (for schema-typed durable
/// rendering and watch resolution), and the pending step mode.
pub struct Debugger {
    /// Lines (1-based) that carry a verified breakpoint, in the entry file. A
    /// statement whose span starts on one of these stops unconditionally.
    breakpoints: Vec<u32>,
    /// The step mode governing the *next* stop, updated each time the run resumes.
    mode: Resume,
    /// Set once at launch to stop before the first statement (`stopOnEntry`).
    stop_on_entry: bool,
    /// True after the entry stop has been taken, so it fires at most once.
    entered: bool,
    program: CheckedProgram,
    events: Sender<RunEvent>,
    control: Receiver<Control>,
}

impl Debugger {
    /// Build the hook for a launch: the verified breakpoint lines, whether to stop
    /// on entry, the program, and the channel ends.
    pub fn new(
        breakpoints: Vec<u32>,
        stop_on_entry: bool,
        program: CheckedProgram,
        events: Sender<RunEvent>,
        control: Receiver<Control>,
    ) -> Self {
        Debugger {
            breakpoints,
            // Until the client steps, run freely to the first breakpoint.
            mode: Resume::Continue,
            stop_on_entry,
            entered: false,
            program,
            events,
            control,
        }
    }

    /// Whether this is the entry stop to take once for `stopOnEntry`.
    fn is_entry_stop(&mut self) -> bool {
        if self.stop_on_entry && !self.entered {
            self.entered = true;
            return true;
        }
        false
    }

    /// Capture the locals at the current frame as owned, `Send` data.
    fn snapshot_locals(frame: &Frame<'_, '_>) -> LocalsSnapshot {
        // Frame::locals yields shadowing outer-then-inner; keep the last binding
        // per name so the value in scope wins, matching what the run sees.
        let mut order: Vec<String> = Vec::new();
        let mut latest: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
        for (name, value) in frame.locals() {
            if !latest.contains_key(name) {
                order.push(name.to_string());
            }
            latest.insert(name.to_string(), value.clone());
        }
        let entries = order
            .into_iter()
            .map(|name| {
                let value = latest.remove(&name).expect("name was inserted");
                LocalVar {
                    name,
                    value: value.display_debug(),
                    expand: variables::is_expandable(&value).then_some(value),
                }
            })
            .collect();
        LocalsSnapshot { entries }
    }

    /// Resolve a watch/REPL expression at the current frame: a `^` path reads the
    /// live store (schema-typed); a bare local name reads the captured locals;
    /// anything else is refused. Read-only by construction.
    fn evaluate(&self, frame: &Frame<'_, '_>, expression: &str) -> Result<String, String> {
        let trimmed = expression.trim();
        if trimmed.starts_with('^') {
            let store = frame.store().borrow();
            return match durable::watch(&*store, &self.program, trimmed) {
                Ok(Some(value)) => Ok(value),
                Ok(None) => Ok("<absent>".to_string()),
                Err(message) => Err(message),
            };
        }
        // A bare identifier resolves against the locals in scope (last binding
        // wins, like the run). Any other shape is not evaluable here.
        if is_bare_name(trimmed) {
            let mut found: Option<String> = None;
            for (name, value) in frame.locals() {
                if name == trimmed {
                    found = Some(value.display_debug());
                }
            }
            return found.ok_or_else(|| format!("no local named `{trimmed}`"));
        }
        Err("only ^ paths are watchable".to_string())
    }

    /// Answer one query from the live frame, returning owned data.
    fn answer(&self, frame: &Frame<'_, '_>, query: Query) -> QueryResult {
        match query {
            Query::Locals => QueryResult::Locals(Self::snapshot_locals(frame)),
            Query::ExpandLocal(value) => QueryResult::Children(variables::children(&value)),
            Query::DurableRoots => {
                let store = frame.store().borrow();
                QueryResult::Durable(durable::roots(&*store))
            }
            Query::DurableChildren(path) => {
                let store = frame.store().borrow();
                QueryResult::Durable(durable::children(&*store, &self.program, &path))
            }
            Query::Evaluate(expression) => {
                QueryResult::Evaluated(self.evaluate(frame, &expression))
            }
        }
    }

    /// Park at a stop: announce it, then serve queries until the protocol thread
    /// resumes or terminates. Returns `Ok(())` to continue the run or `Err` to
    /// unwind it (terminate). The frame is borrowed only here, on this thread.
    fn park(&mut self, info: StopInfo, frame: Frame<'_, '_>) -> Result<(), RuntimeError> {
        if self.events.send(RunEvent::Stopped(info)).is_err() {
            // The protocol thread is gone; unwind the run rather than hang.
            return Err(terminated());
        }
        loop {
            match self.control.recv() {
                Ok(Control::Query(query)) => {
                    let answer = self.answer(&frame, query);
                    if self.events.send(RunEvent::Answer(answer)).is_err() {
                        return Err(terminated());
                    }
                }
                Ok(Control::Resume(mode)) => {
                    self.mode = mode;
                    return Ok(());
                }
                // A terminate, a disconnect, or a dropped channel all unwind.
                Ok(Control::Terminate) | Err(_) => return Err(terminated()),
            }
        }
    }
}

impl StepHook for Debugger {
    fn before_statement(
        &mut self,
        span: marrow_syntax::SourceSpan,
        frame: Frame<'_, '_>,
    ) -> Result<(), RuntimeError> {
        let depth = frame.depth();
        let line_has_breakpoint = self.breakpoints.contains(&span.line);

        let reason = if self.is_entry_stop() {
            Some(StopReason::Entry)
        } else {
            step::decide(self.mode, depth, line_has_breakpoint)
        };

        let Some(reason) = reason else {
            return Ok(());
        };
        let info = StopInfo {
            reason,
            line: span.line,
            column: span.column,
            depth,
        };
        self.park(info, frame)
    }
}

/// The terminate fault returned to unwind the run on a client terminate or a lost
/// protocol thread. It carries the [`RUN_TERMINATED`] code so the surfaced fault
/// is recognizable and any open transaction rolls back as the stack unwinds.
fn terminated() -> RuntimeError {
    RuntimeError {
        code: RUN_TERMINATED,
        message: "run terminated by the debugger".to_string(),
        span: marrow_syntax::SourceSpan::default(),
        throw: None,
        origin: None,
    }
}

/// Whether `text` is a single bare identifier (a local name), as opposed to a
/// path or a larger expression — the only non-`^` shape `evaluate` accepts.
fn is_bare_name(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|c| c.is_alphanumeric() || c == '_')
        && text
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_name_recognizes_identifiers_only() {
        assert!(is_bare_name("total"));
        assert!(is_bare_name("_x1"));
        assert!(!is_bare_name("^books"));
        assert!(!is_bare_name("a.b"));
        assert!(!is_bare_name("1abc"));
        assert!(!is_bare_name(""));
    }

    #[test]
    fn terminated_fault_carries_the_terminated_code() {
        assert_eq!(terminated().code, RUN_TERMINATED);
    }
}
