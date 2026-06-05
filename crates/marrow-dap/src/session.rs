//! The DAP session: the protocol-thread state machine.
//!
//! It owns the sequence counter, the breakpoint table, the variable-reference
//! registry, and the handle to the run-thread. Each client request is handled
//! here, on the protocol thread; inspection requests at a stop are forwarded to
//! the parked run-thread as [`Query`] messages and answered from owned data. The
//! session never borrows the runtime frame — that lives only on the run-thread.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use serde_json::{Value as Json, json};

use crate::debugger::{Control, Query, QueryResult, RunEvent, StopInfo};
use crate::protocol::write_message;
use crate::step::Resume;

/// The variable reference for the Locals scope at a stop. Fixed, since there is
/// one Locals scope per frame.
const LOCALS_REF: i64 = 1;
/// Reserved durable-data reference. It rejects until Marrow exposes canonical
/// durable watch/path facts.
const DURABLE_REF: i64 = 2;
/// The first dynamically-allocated reference for expandable locals.
const FIRST_DYNAMIC_REF: i64 = 1000;
/// The single thread id the debugger exposes — the run is single-threaded.
const THREAD_ID: i64 = 1;
const RAW_DATA_INSPECTION_BLOCKED: &str =
    "blocked-on-marrow: durable-data inspection needs typed durable watch/path facts from Marrow";
const EXPRESSION_EVALUATE_BLOCKED: &str = "blocked-on-marrow: DAP watch/REPL evaluate needs canonical expression/evaluate facts from Marrow";
const LAUNCH_ARGS_BLOCKED: &str =
    "blocked-on-marrow: DAP launch args need typed launch argument decoding facts from Marrow";
const BREAKPOINT_VERIFICATION_BLOCKED: &str =
    "blocked-on-marrow: DAP breakpoint verification needs canonical stop-point facts from Marrow";
const LOCAL_VALUE_BLOCKER: &str = "canonical runtime value expansion facts";

/// What a dynamic variable reference expands to. Resolved on the run-thread.
#[derive(Clone)]
enum Expandable {
    Local(marrow_run::Value),
}

/// The live run-thread plus the channels to drive it.
struct Running {
    handle: JoinHandle<Option<crate::run::Outcome>>,
    control: Sender<Control>,
    events: Receiver<RunEvent>,
    /// The most recent stop, kept so `stackTrace`/`scopes` answer without
    /// re-querying. `None` while the run is executing (not parked).
    stopped_at: Option<StopInfo>,
}

/// The protocol-thread session.
pub struct Session<W: Write> {
    out: W,
    seq: i64,
    /// Unverified requested breakpoint lines. They are line-only transport until
    /// Marrow exposes canonical debugger stop-point and source facts.
    breakpoints: Vec<u32>,
    /// The run-thread, present once launched and configurationDone-d.
    running: Option<Running>,
    /// Pending launch arguments captured between `launch` and `configurationDone`
    /// (DAP defers the actual start until breakpoints are set).
    pending: Option<PendingLaunch>,
    /// The dynamic reference registry for the current stop, cleared on resume.
    expandables: HashMap<i64, Expandable>,
    next_ref: i64,
    /// Set once the client asked to disconnect/terminate, so the loop exits.
    done: bool,
}

/// Launch arguments held until `configurationDone` starts the run.
struct PendingLaunch {
    project_dir: PathBuf,
    entry: Option<String>,
    args: Vec<marrow_run::Value>,
    stop_on_entry: bool,
}

impl<W: Write> Session<W> {
    pub fn new(out: W) -> Self {
        Session {
            out,
            seq: 0,
            breakpoints: Vec::new(),
            running: None,
            pending: None,
            expandables: HashMap::new(),
            next_ref: FIRST_DYNAMIC_REF,
            done: false,
        }
    }

    /// Whether the client has asked to end the session.
    pub fn done(&self) -> bool {
        self.done
    }

    /// The next outgoing sequence number.
    fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }

    /// Send a DAP response to a request.
    fn respond(&mut self, request: &Json, success: bool, body: Json) {
        let seq = self.next_seq();
        let mut response = json!({
            "seq": seq,
            "type": "response",
            "request_seq": request.get("seq").cloned().unwrap_or(Json::Null),
            "success": success,
            "command": request.get("command").cloned().unwrap_or(Json::Null),
        });
        if success {
            response["body"] = body;
        } else if let Some(message) = body.as_str() {
            response["message"] = json!(message);
        }
        write_message(&mut self.out, &response);
    }

    /// Send a DAP event.
    fn event(&mut self, name: &str, body: Json) {
        let seq = self.next_seq();
        let event = json!({
            "seq": seq,
            "type": "event",
            "event": name,
            "body": body,
        });
        write_message(&mut self.out, &event);
    }

    /// Allocate a dynamic variable reference for an expandable value.
    fn alloc_ref(&mut self, expandable: Expandable) -> i64 {
        let reference = self.next_ref;
        self.next_ref += 1;
        self.expandables.insert(reference, expandable);
        reference
    }

    /// Handle one decoded client request, dispatching by `command`.
    pub fn handle(&mut self, request: &Json) {
        let command = request
            .get("command")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string();
        let arguments = request
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        match command.as_str() {
            "initialize" => self.on_initialize(request),
            "launch" => self.on_launch(request, &arguments),
            "setBreakpoints" => self.on_set_breakpoints(request, &arguments),
            "configurationDone" => self.on_configuration_done(request),
            "threads" => self.on_threads(request),
            "stackTrace" => self.on_stack_trace(request),
            "scopes" => self.on_scopes(request),
            "variables" => self.on_variables(request, &arguments),
            "continue" => self.on_continue(request),
            "next" => self.on_step(request, StepKind::Over),
            "stepIn" => self.on_step(request, StepKind::Into),
            "stepOut" => self.on_step(request, StepKind::Out),
            "pause" => self.on_pause(request),
            "evaluate" => self.on_evaluate(request, &arguments),
            "disconnect" | "terminate" => self.on_disconnect(request),
            other => self.respond(
                request,
                false,
                json!(format!("unsupported request `{other}`")),
            ),
        }
    }

    /// `initialize`: advertise the small capability set the debugger supports,
    /// then announce readiness with the `initialized` event so the client sends
    /// breakpoints.
    fn on_initialize(&mut self, request: &Json) {
        self.respond(
            request,
            true,
            json!({
                "supportsConfigurationDoneRequest": true,
                "supportsTerminateRequest": true,
                "supportsEvaluateForHovers": false,
            }),
        );
        self.event("initialized", json!({}));
    }

    /// `launch`: validate and stash the launch arguments. The run does not start
    /// until `configurationDone`, so breakpoints set in between take effect.
    fn on_launch(&mut self, request: &Json, arguments: &Json) {
        let Some(project) = arguments.get("project").and_then(Json::as_str) else {
            self.respond(request, false, json!("launch needs a `project` directory"));
            return;
        };
        let project_dir = PathBuf::from(project);
        let entry = arguments
            .get("entry")
            .and_then(Json::as_str)
            .map(str::to_string);
        let args = match parse_args(arguments.get("args")) {
            Ok(args) => args,
            Err(message) => {
                self.respond(request, false, json!(message));
                return;
            }
        };
        let stop_on_entry = arguments
            .get("stopOnEntry")
            .and_then(Json::as_bool)
            .unwrap_or(false);
        // Prepare the project now so a bad project fails the launch with a clear
        // message, but keep the store/program until configurationDone starts it.
        match crate::project::prepare(&project_dir, entry.as_deref()) {
            Ok(_) => {
                self.pending = Some(PendingLaunch {
                    project_dir,
                    entry,
                    args,
                    stop_on_entry,
                });
                self.respond(request, true, json!({}));
            }
            Err(error) => self.respond(request, false, json!(error.to_string())),
        }
    }

    /// `setBreakpoints`: keep each requested source line as an unverified runtime
    /// stop until Marrow provides canonical stop-point facts.
    fn on_set_breakpoints(&mut self, request: &Json, arguments: &Json) {
        let has_source_path = arguments
            .get("source")
            .and_then(|source| source.get("path"))
            .and_then(Json::as_str)
            .is_some();
        let requested: Vec<u32> = arguments
            .get("breakpoints")
            .and_then(Json::as_array)
            .map(|points| {
                points
                    .iter()
                    .filter_map(|point| point.get("line").and_then(Json::as_u64))
                    .map(|line| line as u32)
                    .collect()
            })
            .unwrap_or_default();

        if !has_source_path {
            self.respond(request, true, json!({ "breakpoints": [] }));
            return;
        }

        let advisory = self.resolve_breakpoints(&requested);
        self.breakpoints = requested;
        self.respond(request, true, json!({ "breakpoints": advisory }));
    }

    /// Return unverified breakpoint objects for the requested source lines.
    fn resolve_breakpoints(&self, requested: &[u32]) -> Vec<Json> {
        requested
            .iter()
            .map(|line| {
                json!({
                    "verified": false,
                    "line": *line,
                    "message": BREAKPOINT_VERIFICATION_BLOCKED,
                })
            })
            .collect()
    }

    /// `configurationDone`: start the run-thread with the captured launch and the
    /// unverified breakpoint lines. The run begins immediately; the first stop
    /// arrives as a `stopped` event the dispatcher pumps.
    fn on_configuration_done(&mut self, request: &Json) {
        let Some(pending) = self.pending.take() else {
            self.respond(request, false, json!("configurationDone before a launch"));
            return;
        };
        let breakpoint_lines = self.unverified_breakpoint_lines();
        match crate::run::spawn(
            pending.project_dir,
            pending.entry,
            pending.args,
            breakpoint_lines,
            pending.stop_on_entry,
        ) {
            Ok(running) => {
                self.running = Some(Running {
                    handle: running.handle,
                    control: running.control,
                    events: running.events,
                    stopped_at: None,
                });
                self.respond(request, true, json!({}));
                self.pump_until_stopped_or_done();
            }
            Err(error) => {
                self.respond(request, false, json!(error));
                self.terminate_session();
            }
        }
    }

    /// The requested breakpoint lines, not tied to a source file until Marrow
    /// exposes canonical debugger source facts.
    fn unverified_breakpoint_lines(&self) -> Vec<u32> {
        self.breakpoints.clone()
    }

    /// `threads`: the one synthetic thread the single-threaded run presents.
    fn on_threads(&mut self, request: &Json) {
        self.respond(
            request,
            true,
            json!({ "threads": [{ "id": THREAD_ID, "name": "marrow" }] }),
        );
    }

    /// `stackTrace`: the single current frame at the stop. A statement debugger
    /// reports the stopped statement's location; deeper callers are summarized by
    /// the activation depth rather than reconstructed.
    fn on_stack_trace(&mut self, request: &Json) {
        let Some(stop) = self.running.as_ref().and_then(|r| r.stopped_at.as_ref()) else {
            self.respond(
                request,
                true,
                json!({ "stackFrames": [], "totalFrames": 0 }),
            );
            return;
        };
        let name = format!("depth {}", stop.depth);
        let frame = json!({
            "id": THREAD_ID,
            "name": name,
            "line": stop.line,
            "column": stop.column.max(1),
        });
        self.respond(
            request,
            true,
            json!({ "stackFrames": [frame], "totalFrames": 1 }),
        );
    }

    /// `scopes`: Locals are available. Durable data is blocked until Marrow
    /// exposes canonical tooling facts.
    fn on_scopes(&mut self, request: &Json) {
        let scopes = vec![json!({
            "name": "Locals",
            "variablesReference": LOCALS_REF,
            "expensive": false,
        })];
        self.respond(request, true, json!({ "scopes": scopes }));
    }

    /// `variables`: list the children of a scope or an expandable local value.
    fn on_variables(&mut self, request: &Json, arguments: &Json) {
        let reference = arguments
            .get("variablesReference")
            .and_then(Json::as_i64)
            .unwrap_or(0);
        if reference == DURABLE_REF {
            self.respond(request, false, json!(RAW_DATA_INSPECTION_BLOCKED));
            return;
        }
        let variables = match reference {
            LOCALS_REF => self.locals_variables(),
            other => match self.expandables.get(&other).cloned() {
                Some(Expandable::Local(value)) => self.expand_local(value),
                None => Vec::new(),
            },
        };
        self.respond(request, true, json!({ "variables": variables }));
    }

    /// The Locals scope contents: each local rendered, with a reference for any
    /// compound value the client can drill into.
    fn locals_variables(&mut self) -> Vec<Json> {
        let Some(QueryResult::Locals(snapshot)) = self.query(Query::Locals) else {
            return Vec::new();
        };
        snapshot
            .entries
            .into_iter()
            .map(|local| {
                let reference = local
                    .expand
                    .map(|value| self.alloc_ref(Expandable::Local(value)))
                    .unwrap_or(0);
                variable_json(&local.name, &local.value, reference, LOCAL_VALUE_BLOCKER)
            })
            .collect()
    }

    /// Expand a compound local value (a sequence, resource, or identity).
    fn expand_local(&mut self, value: marrow_run::Value) -> Vec<Json> {
        let Some(QueryResult::Children(children)) = self.query(Query::ExpandLocal(value)) else {
            return Vec::new();
        };
        children
            .into_iter()
            .map(|child| {
                let reference = child
                    .expand
                    .map(|value| self.alloc_ref(Expandable::Local(value)))
                    .unwrap_or(0);
                variable_json(&child.name, &child.value, reference, LOCAL_VALUE_BLOCKER)
            })
            .collect()
    }

    /// `continue`: resume until the next breakpoint.
    fn on_continue(&mut self, request: &Json) {
        if self.running.is_none() {
            self.respond(request, false, json!("not running"));
            return;
        }
        self.respond(request, true, json!({ "allThreadsContinued": true }));
        self.event(
            "continued",
            json!({ "threadId": THREAD_ID, "allThreadsContinued": true }),
        );
        self.resume_run(Resume::Continue);
    }

    /// `next` / `stepIn` / `stepOut`: resume with the matching depth-relative step
    /// target, computed from the depth at the current stop.
    fn on_step(&mut self, request: &Json, kind: StepKind) {
        let Some(depth) = self
            .running
            .as_ref()
            .and_then(|r| r.stopped_at.as_ref())
            .map(|stop| stop.depth)
        else {
            self.respond(request, false, json!("not stopped"));
            return;
        };
        let mode = match kind {
            StepKind::Over => Resume::Over { from_depth: depth },
            StepKind::Into => Resume::Into,
            StepKind::Out => Resume::Out { from_depth: depth },
        };
        self.respond(request, true, json!({}));
        self.resume_run(mode);
    }

    /// `pause`: stop at the next statement.
    ///
    /// The protocol loop pumps run events synchronously, so it can read a request
    /// only while the run is parked at a stop — a pause therefore takes effect by
    /// resuming with the [`Resume::Pause`] mode, which stops before the very next
    /// statement (and reports `pause`). A client that is already stopped sees a
    /// one-statement advance; this is the honest behavior of a synchronous pump,
    /// which cannot interrupt a statement mid-flight.
    fn on_pause(&mut self, request: &Json) {
        if self.running.is_none() {
            self.respond(request, false, json!("not running"));
            return;
        }
        self.respond(request, true, json!({}));
        self.event(
            "continued",
            json!({ "threadId": THREAD_ID, "allThreadsContinued": true }),
        );
        self.resume_run(Resume::Pause);
    }

    /// `evaluate`: watch/REPL requests are blocked until Marrow exposes canonical
    /// expression and durable path facts.
    fn on_evaluate(&mut self, request: &Json, arguments: &Json) {
        if arguments.get("context").and_then(Json::as_str) == Some("hover") {
            self.respond(
                request,
                false,
                json!(
                    "blocked-on-marrow: DAP hover evaluate needs canonical evaluate facts from Marrow"
                ),
            );
            return;
        }

        let expression = arguments
            .get("expression")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string();
        if !expression.trim_start().starts_with('^') {
            self.respond(request, false, json!(EXPRESSION_EVALUATE_BLOCKED));
            return;
        }
        self.respond(request, false, json!(RAW_DATA_INSPECTION_BLOCKED));
    }

    /// `disconnect` / `terminate`: unwind the run (rolling back any open
    /// transaction) and end the session.
    fn on_disconnect(&mut self, request: &Json) {
        self.respond(request, true, json!({}));
        self.terminate_session();
    }

    /// Send a query to the parked run-thread and wait for its owned answer. The
    /// run-thread is parked inside `before_statement` whenever the protocol thread
    /// is handling an inspection request, so the answer comes promptly. Returns
    /// `None` when the run is not parked or the channel dropped.
    fn query(&mut self, query: Query) -> Option<QueryResult> {
        let running = self.running.as_ref()?;
        running.stopped_at.as_ref()?;
        running.control.send(Control::Query(query)).ok()?;
        match running.events.recv() {
            Ok(RunEvent::Answer(answer)) => Some(answer),
            // A stop event cannot arrive mid-query (the run is parked), and any
            // other outcome means the run-thread is gone.
            _ => None,
        }
    }

    /// Resume the parked run with `mode`, clear the stop-scoped reference table,
    /// then pump until the next stop or the run's end.
    fn resume_run(&mut self, mode: Resume) {
        self.expandables.clear();
        if let Some(running) = self.running.as_mut() {
            running.stopped_at = None;
            if running.control.send(Control::Resume(mode)).is_err() {
                // The run-thread is gone; finish the session.
                self.finish_run();
                return;
            }
        }
        self.pump_until_stopped_or_done();
    }

    /// Read run-thread events until the run parks at a stop (then return, leaving
    /// the session waiting for the client) or finishes (then emit terminated/exited
    /// and clean up). Output events are forwarded as they arrive.
    fn pump_until_stopped_or_done(&mut self) {
        loop {
            let event = match self.running.as_ref() {
                Some(running) => running.events.recv(),
                None => return,
            };
            match event {
                Ok(RunEvent::Stopped(info)) => {
                    let reason = info.reason;
                    if let Some(running) = self.running.as_mut() {
                        running.stopped_at = Some(info);
                    }
                    self.event(
                        "stopped",
                        json!({
                            "reason": reason.as_str(),
                            "threadId": THREAD_ID,
                            "allThreadsStopped": true,
                        }),
                    );
                    return;
                }
                // An Answer here would be a protocol-thread bug (we only recv
                // answers inside `query`); ignore and keep pumping.
                Ok(RunEvent::Answer(_)) => continue,
                // The events channel closed: the run-thread finished. Collect its
                // outcome and end the session.
                Err(_) => {
                    self.finish_run();
                    return;
                }
            }
        }
    }

    /// Join the finished run-thread, emit its captured output and outcome, and
    /// mark the session done.
    fn finish_run(&mut self) {
        if let Some(running) = self.running.take() {
            let outcome = running.handle.join().ok().flatten();
            if let Some(outcome) = outcome {
                if !outcome.output.is_empty() {
                    self.event(
                        "output",
                        json!({ "category": "stdout", "output": outcome.output }),
                    );
                }
                match outcome.result {
                    Ok(message) => {
                        if !message.is_empty() {
                            self.event(
                                "output",
                                json!({ "category": "stdout", "output": format!("{message}\n") }),
                            );
                        }
                        self.event("exited", json!({ "exitCode": 0 }));
                    }
                    Err(message) => {
                        self.event(
                            "output",
                            json!({ "category": "stderr", "output": format!("{message}\n") }),
                        );
                        self.event("exited", json!({ "exitCode": 1 }));
                    }
                }
            }
            self.event("terminated", json!({}));
        }
        self.done = true;
    }

    /// Terminate the run-thread (if any) and mark the session done. Sends a
    /// terminate control so the hook unwinds, then drains to completion.
    fn terminate_session(&mut self) {
        if let Some(running) = self.running.as_ref() {
            let _ = running.control.send(Control::Terminate);
        }
        // Drain any final events and join.
        self.pump_until_stopped_or_done();
        if self.running.is_some() {
            self.finish_run();
        }
        self.done = true;
    }
}

/// Which depth-relative step a client requested.
enum StepKind {
    Over,
    Into,
    Out,
}

/// Build a DAP `Variable` object. A zero reference marks a leaf; a non-zero one
/// lets the client expand it.
fn variable_json(name: &str, value: &str, reference: i64, blocked_on: &str) -> Json {
    json!({
        "name": name,
        "value": value,
        "presentationHint": debug_admin_presentation_hint(),
        "marrowContract": debug_admin_contract(blocked_on),
        "variablesReference": reference,
    })
}

fn debug_admin_contract(blocked_on: &str) -> Json {
    json!({
        "status": "blocked-on-marrow",
        "blockedOn": blocked_on,
    })
}

fn debug_admin_presentation_hint() -> Json {
    json!({
        "kind": "data",
        "attributes": ["readOnly"],
    })
}

/// Parse the optional launch `args` array. Non-empty values are blocked until
/// Marrow exposes typed launch argument facts.
fn parse_args(args: Option<&Json>) -> Result<Vec<marrow_run::Value>, String> {
    let Some(array) = args else {
        return Ok(Vec::new());
    };
    let Some(items) = array.as_array() else {
        return Err("`args` must be an array".to_string());
    };
    if !items.is_empty() {
        return Err(LAUNCH_ARGS_BLOCKED.to_string());
    }
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_breakpoints_keeps_latest_unverified_lines_without_source_identity() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("m.mw");
        let other_file = dir.path().join("other.mw");
        std::fs::write(&file, "module m\n\npub fn f(): int\n    return 1\n").unwrap();
        std::fs::write(
            &other_file,
            "module other\n\npub fn f(): int\n    return 2\n",
        )
        .unwrap();
        let mut session = Session::new(Vec::new());
        let request = json!({ "seq": 1, "command": "setBreakpoints" });

        session.on_set_breakpoints(
            &request,
            &json!({
                "source": { "path": file.display().to_string() },
                "breakpoints": [{ "line": 3 }]
            }),
        );
        session.on_set_breakpoints(
            &request,
            &json!({
                "source": { "path": other_file.display().to_string() },
                "breakpoints": [{ "line": 4 }]
            }),
        );

        assert_eq!(session.unverified_breakpoint_lines(), vec![4]);
    }

    #[test]
    fn breakpoint_response_reports_unverified_source_line() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("m.mw");
        std::fs::write(&file, "module m\n\npub fn f(): int\n    return 1\n").unwrap();
        let mut session = Session::new(Vec::new());
        let request = json!({ "seq": 1, "command": "setBreakpoints" });

        session.on_set_breakpoints(
            &request,
            &json!({
                "source": { "path": file.display().to_string() },
                "breakpoints": [{ "line": 3 }]
            }),
        );

        let output = String::from_utf8(session.out).unwrap();
        let (_, body) = output.split_once("\r\n\r\n").unwrap();
        let response: Json = serde_json::from_str(body).unwrap();
        let breakpoint = &response["body"]["breakpoints"][0];
        assert_eq!(breakpoint["verified"], false, "{response}");
        assert_eq!(breakpoint["line"], 3, "{response}");
        let message = breakpoint["message"].as_str().unwrap();
        assert!(
            message.contains("blocked-on-marrow"),
            "breakpoint response should keep the Marrow blocker: {response}"
        );
    }
}
