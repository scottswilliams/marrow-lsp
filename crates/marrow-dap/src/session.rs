//! The DAP session: the protocol-thread state machine.
//!
//! It owns the sequence counter, the breakpoint table, the variable-reference
//! registry, and the handle to the run-thread. Each client request is handled
//! here, on the protocol thread; inspection requests at a stop are forwarded to
//! the parked run-thread as [`Query`] messages and answered from owned data. The
//! session never borrows the runtime frame — that lives only on the run-thread.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use serde_json::{Value as Json, json};

use marrow_check::CheckedProgram;
use marrow_store::path::PathSegment;

use crate::breakpoints;
use crate::debugger::{Control, Query, QueryResult, RunEvent, StopInfo};
use crate::protocol::write_message;
use crate::step::Resume;

/// The variable reference for the Locals scope at a stop. Fixed, since there is
/// one Locals scope per frame.
const LOCALS_REF: i64 = 1;
/// The variable reference for the opt-in raw durable-data scope. Fixed likewise.
const DURABLE_REF: i64 = 2;
/// The first dynamically-allocated reference, for expandable locals and durable
/// subtrees. Kept clear of the two fixed scope references.
const FIRST_DYNAMIC_REF: i64 = 1000;
/// The single thread id the debugger exposes — the run is single-threaded.
const THREAD_ID: i64 = 1;
const RAW_DATA_INSPECTION_BLOCKED: &str = "blocked-on-marrow: raw durable-data inspection needs typed durable watch/path facts from Marrow";

/// What a dynamic variable reference expands to. Resolved on the run-thread: a
/// local value is expanded in memory, a durable path is read from the live store.
#[derive(Clone)]
enum Expandable {
    Local(marrow_run::Value),
    Durable(Vec<PathSegment>),
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
    /// Requested breakpoint lines per source file. They are resolved against the
    /// loaded program when answering `setBreakpoints` and again when the run starts,
    /// so breakpoints sent before `launch` are not lost.
    breakpoints: HashMap<PathBuf, Vec<u32>>,
    /// The program for the launched project, for breakpoint resolution. Set at
    /// launch.
    program: Option<CheckedProgram>,
    /// The entry source file, the one the debugger reports stack frames in.
    entry_file: Option<PathBuf>,
    /// The run-thread, present once launched and configurationDone-d.
    running: Option<Running>,
    /// Pending launch arguments captured between `launch` and `configurationDone`
    /// (DAP defers the actual start until breakpoints are set).
    pending: Option<PendingLaunch>,
    /// The dynamic reference registry for the current stop, cleared on resume.
    expandables: HashMap<i64, Expandable>,
    next_ref: i64,
    /// Launch opt-in for raw durable `^` inspection. Program execution still
    /// uses the configured store; this gates only debugger inspection surfaces.
    allow_raw_data_inspection: bool,
    /// Set once the client asked to disconnect/terminate, so the loop exits.
    done: bool,
}

/// Launch arguments held until `configurationDone` starts the run.
struct PendingLaunch {
    project_dir: PathBuf,
    entry: Option<String>,
    args: Vec<marrow_run::Value>,
    stop_on_entry: bool,
    allow_raw_data_inspection: bool,
}

impl<W: Write> Session<W> {
    pub fn new(out: W) -> Self {
        Session {
            out,
            seq: 0,
            breakpoints: HashMap::new(),
            program: None,
            entry_file: None,
            running: None,
            pending: None,
            expandables: HashMap::new(),
            next_ref: FIRST_DYNAMIC_REF,
            allow_raw_data_inspection: false,
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
        let allow_raw_data_inspection = arguments
            .get("allowRawDataInspection")
            .and_then(Json::as_bool)
            .unwrap_or(false);

        // Prepare the project now so a bad project fails the launch with a clear
        // message, but keep the store/program until configurationDone starts it.
        match crate::project::prepare(&project_dir, entry.as_deref()) {
            Ok(launch) => {
                self.program = Some(launch.program.clone());
                self.entry_file = entry_source_file(&launch.program, &launch.entry);
                self.pending = Some(PendingLaunch {
                    project_dir,
                    entry,
                    args,
                    stop_on_entry,
                    allow_raw_data_inspection,
                });
                self.allow_raw_data_inspection = allow_raw_data_inspection;
                self.respond(request, true, json!({}));
            }
            Err(error) => self.respond(request, false, json!(error.to_string())),
        }
    }

    /// `setBreakpoints`: snap each requested line to the next statement in the
    /// source file and verify it back. Lines that fall past the last statement are
    /// returned unverified.
    fn on_set_breakpoints(&mut self, request: &Json, arguments: &Json) {
        let path = arguments
            .get("source")
            .and_then(|source| source.get("path"))
            .and_then(Json::as_str)
            .map(PathBuf::from);
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

        let Some(path) = path else {
            self.respond(request, true, json!({ "breakpoints": [] }));
            return;
        };

        let (verified, _) = self.resolve_breakpoints(&path, &requested);
        self.breakpoints.insert(path, requested);
        self.respond(request, true, json!({ "breakpoints": verified }));
    }

    /// Resolve requested lines against the program's statement lines for `path`,
    /// returning the per-breakpoint client objects and the resolved line set.
    fn resolve_breakpoints(&self, path: &Path, requested: &[u32]) -> (Vec<Json>, Vec<u32>) {
        let lines = self
            .program
            .as_ref()
            .map(|program| breakpoints::statement_lines(program, path))
            .unwrap_or_default();
        let mut verified = Vec::new();
        let mut resolved = Vec::new();
        for &line in requested {
            match breakpoints::resolve_line(&lines, line) {
                Some(stmt_line) => {
                    resolved.push(stmt_line);
                    verified.push(json!({ "verified": true, "line": stmt_line }));
                }
                None => verified.push(json!({ "verified": false, "line": line })),
            }
        }
        (verified, resolved)
    }

    /// `configurationDone`: start the run-thread with the captured launch and the
    /// resolved breakpoints. The run begins immediately; the first stop arrives as
    /// a `stopped` event the dispatcher pumps.
    fn on_configuration_done(&mut self, request: &Json) {
        let Some(pending) = self.pending.take() else {
            self.respond(request, false, json!("configurationDone before a launch"));
            return;
        };
        let breakpoint_lines = self.entry_breakpoint_lines();
        let allow_raw_data_inspection = pending.allow_raw_data_inspection;
        match crate::run::spawn(
            pending.project_dir,
            pending.entry,
            pending.args,
            breakpoint_lines,
            pending.stop_on_entry,
        ) {
            Ok(running) => {
                self.allow_raw_data_inspection = allow_raw_data_inspection;
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

    /// The verified breakpoint lines in the entry source file — the file the run
    /// reports frames in and the runtime spans line up with.
    fn entry_breakpoint_lines(&self) -> Vec<u32> {
        match &self.entry_file {
            Some(file) => self
                .breakpoints
                .get(file)
                .map(|requested| self.resolved_breakpoint_lines(file, requested))
                .unwrap_or_default(),
            // No known entry file: union every set breakpoint line, since a
            // single-file project's spans still match by line.
            None => self
                .breakpoints
                .iter()
                .flat_map(|(file, requested)| self.resolved_breakpoint_lines(file, requested))
                .collect(),
        }
    }

    fn resolved_breakpoint_lines(&self, path: &Path, requested: &[u32]) -> Vec<u32> {
        let lines = self
            .program
            .as_ref()
            .map(|program| breakpoints::statement_lines(program, path))
            .unwrap_or_default();
        requested
            .iter()
            .filter_map(|line| breakpoints::resolve_line(&lines, *line))
            .collect()
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
        let path = self
            .entry_file
            .as_ref()
            .map(|file| file.display().to_string())
            .unwrap_or_default();
        let name = format!("depth {}", stop.depth);
        let frame = json!({
            "id": THREAD_ID,
            "name": name,
            "line": stop.line,
            "column": stop.column.max(1),
            "source": { "path": path },
        });
        self.respond(
            request,
            true,
            json!({ "stackFrames": [frame], "totalFrames": 1 }),
        );
    }

    /// `scopes`: Locals are always available. Raw durable data is a debug/admin
    /// prototype and is exposed only when launch opts in.
    fn on_scopes(&mut self, request: &Json) {
        let mut scopes = vec![json!({
            "name": "Locals",
            "variablesReference": LOCALS_REF,
            "expensive": false,
        })];
        if self.allow_raw_data_inspection {
            scopes.push(json!({
                "name": "^ Durable Data (debug/admin prototype)",
                "variablesReference": DURABLE_REF,
                "expensive": true,
            }));
        }
        self.respond(request, true, json!({ "scopes": scopes }));
    }

    /// `variables`: list the children of a scope or an expandable value, querying
    /// the parked run-thread for live data.
    fn on_variables(&mut self, request: &Json, arguments: &Json) {
        let reference = arguments
            .get("variablesReference")
            .and_then(Json::as_i64)
            .unwrap_or(0);
        if !self.allow_raw_data_inspection
            && (reference == DURABLE_REF
                || matches!(
                    self.expandables.get(&reference),
                    Some(Expandable::Durable(_))
                ))
        {
            self.respond(request, false, json!(RAW_DATA_INSPECTION_BLOCKED));
            return;
        }
        let variables = match reference {
            LOCALS_REF => self.locals_variables(),
            DURABLE_REF => self.durable_variables(None),
            other => match self.expandables.get(&other).cloned() {
                Some(Expandable::Local(value)) => self.expand_local(value),
                Some(Expandable::Durable(path)) => self.durable_variables(Some(path)),
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
                variable_json(&local.name, &local.value, reference)
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
                variable_json(&child.name, &child.value, reference)
            })
            .collect()
    }

    /// The durable scope contents: the saved roots, or the children of `path`.
    fn durable_variables(&mut self, path: Option<Vec<PathSegment>>) -> Vec<Json> {
        let query = match path {
            Some(path) => Query::DurableChildren(path),
            None => Query::DurableRoots,
        };
        let Some(QueryResult::Durable(nodes)) = self.query(query) else {
            return Vec::new();
        };
        nodes
            .into_iter()
            .map(|node| {
                let reference = if node.expandable {
                    self.alloc_ref(Expandable::Durable(node.path))
                } else {
                    0
                };
                variable_json(&node.name, &node.value, reference)
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

    /// `evaluate`: a watch/REPL request. Restricted to read-only `^` paths and
    /// bare local names; anything else is refused with a clear message.
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
        if !self.allow_raw_data_inspection && expression.trim_start().starts_with('^') {
            self.respond(request, false, json!(RAW_DATA_INSPECTION_BLOCKED));
            return;
        }
        let Some(QueryResult::Evaluated(result)) = self.query(Query::Evaluate(expression)) else {
            self.respond(request, false, json!("not stopped"));
            return;
        };
        match result {
            Ok(value) => self.respond(
                request,
                true,
                json!({ "result": value, "variablesReference": 0 }),
            ),
            Err(message) => self.respond(request, false, json!(message)),
        }
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
fn variable_json(name: &str, value: &str, reference: i64) -> Json {
    json!({
        "name": name,
        "value": value,
        "variablesReference": reference,
    })
}

/// Parse the optional launch `args` array into scalar runtime values. Only
/// scalars are accepted (the entry's parameters are scalars); a non-scalar is a
/// launch error.
fn parse_args(args: Option<&Json>) -> Result<Vec<marrow_run::Value>, String> {
    let Some(array) = args else {
        return Ok(Vec::new());
    };
    let Some(items) = array.as_array() else {
        return Err("`args` must be an array of scalars".to_string());
    };
    items.iter().map(scalar_arg).collect()
}

/// One launch argument as a scalar [`marrow_run::Value`].
fn scalar_arg(arg: &Json) -> Result<marrow_run::Value, String> {
    match arg {
        Json::Bool(value) => Ok(marrow_run::Value::Bool(*value)),
        Json::String(text) => Ok(marrow_run::Value::Str(text.clone())),
        Json::Number(number) => number
            .as_i64()
            .map(marrow_run::Value::Int)
            .ok_or_else(|| "only integer numbers are supported as args".to_string()),
        other => Err(format!("unsupported arg `{other}`; pass scalars only")),
    }
}

/// The source file the named entry function lives in, so stack frames and entry
/// breakpoints reference the right file.
fn entry_source_file(program: &CheckedProgram, entry: &str) -> Option<PathBuf> {
    // The entry is "module::fn" or a bare "fn" searched across modules.
    let (module_name, fn_name) = match entry.rsplit_once("::") {
        Some((module, name)) => (Some(module), name),
        None => (None, entry),
    };
    program
        .modules
        .iter()
        .find(|module| {
            module
                .functions
                .iter()
                .any(|function| function.name == fn_name)
                && module_name.is_none_or(|name| module.name == name)
        })
        .map(|module| module.source_file.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_check::{ProjectSources, analyze_project};
    use marrow_project::ProjectConfig;

    fn program(source: &str) -> (CheckedProgram, PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("m.mw");
        std::fs::write(&file, source).unwrap();
        let config = ProjectConfig {
            source_roots: vec!["src".to_string()],
            default_entry: None,
            store: None,
            tests: Vec::new(),
        };
        let snapshot = analyze_project(&root, &config, &ProjectSources::new()).unwrap();
        (snapshot.program, file, dir)
    }

    #[test]
    fn breakpoints_set_before_launch_are_resolved_when_the_program_arrives() {
        let source = "module m\n\npub fn f(): int\n    return 1\n";
        let (program, file, _dir) = program(source);
        let mut session = Session::new(Vec::new());
        let request = json!({ "seq": 1, "command": "setBreakpoints" });

        session.on_set_breakpoints(
            &request,
            &json!({
                "source": { "path": file.display().to_string() },
                "breakpoints": [{ "line": 3 }]
            }),
        );
        session.program = Some(program);
        session.entry_file = Some(file);

        assert_eq!(session.entry_breakpoint_lines(), vec![4]);
    }
}
