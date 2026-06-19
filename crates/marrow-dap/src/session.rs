//! The DAP session: the protocol-thread state machine.
//!
//! It owns the sequence counter, the variable-reference registry, and the handle
//! to the run-thread. Each client request is handled here, on the protocol
//! thread; inspection requests at a stop are forwarded to the parked run-thread
//! as [`Query`] messages and answered from owned data. The session never borrows
//! the runtime frame — that lives only on the run-thread.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use marrow_run::{EntryArgument, EntryInvocation};
use serde_json::{Value as Json, json};

use crate::debugger::{ArmedBreakpoints, Control, Query, QueryResult, RunEvent, StopInfo};
use crate::protocol::write_message;
use crate::step::Resume;
use crate::variables::{ChildCounts, ChildPage, VariablesFilter};

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
/// The single stack frame id exposed while stopped. It is a frame identity, not
/// the thread identity, even while both singleton ids share the same value.
const FRAME_ID: i64 = 1;
const STATUS_BLOCKED_ON_MARROW: &str = "blocked-on-marrow";
const STATUS_INVALID_PARAMS: &str = "invalid-params";
const STATUS_INVALID_STATE: &str = "invalid-state";
const STATUS_RUNTIME_ERROR: &str = "runtime-error";
const STATUS_UNSUPPORTED_REQUEST: &str = "unsupported-request";
const STOP_POINT_FACTS: &str = "canonical stop-point facts";
const BREAKPOINT_EXPRESSION_FACTS: &str = "canonical stop-point and breakpoint expression facts";
const EXPRESSION_EVALUATE_FACTS: &str = "canonical expression/evaluate facts";
const HOVER_EVALUATE_FACTS: &str = "canonical evaluate facts";
const DURABLE_WATCH_PATH_FACTS: &str = "typed durable watch/path facts";
const RAW_DATA_INSPECTION_BLOCKED: &str =
    "blocked-on-marrow: durable-data inspection needs typed durable watch/path facts from Marrow";
const EXPRESSION_EVALUATE_BLOCKED: &str = "blocked-on-marrow: DAP watch/REPL evaluate needs canonical expression/evaluate facts from Marrow";
const BREAKPOINT_VERIFICATION_BLOCKED: &str =
    "blocked-on-marrow: DAP breakpoint verification needs canonical stop-point facts from Marrow";
const BREAKPOINT_EXPRESSION_BLOCKED: &str = "blocked-on-marrow: DAP conditional/hit-condition/logpoint breakpoints need canonical stop-point and breakpoint expression facts from Marrow";
const BREAKPOINT_SOURCE_INVALID: &str = "missing or invalid breakpoint source";
const BREAKPOINT_SOURCE_REFERENCE_UNSUPPORTED: &str =
    "DAP sourceReference breakpoint sources are not supported";
const BREAKPOINTS_INVALID: &str = "`breakpoints` must be an array";
const BREAKPOINT_INVALID_LINE: &str = "missing or invalid breakpoint line";
const BREAKPOINT_NO_STOP_POINT: &str = "no executable Marrow stop point at this source line";
const HOVER_EVALUATE_BLOCKED: &str =
    "blocked-on-marrow: DAP hover evaluate needs canonical evaluate facts from Marrow";
const EVALUATE_EXPRESSION_INVALID: &str = "missing or invalid evaluate expression";
const EVALUATE_CONTEXT_INVALID: &str = "invalid evaluate context";
const VARIABLES_REFERENCE_INVALID: &str = "missing or invalid variablesReference";
const VARIABLES_PAGE_INVALID: &str = "invalid variables paging arguments";
const VARIABLES_FILTER_INVALID: &str = "invalid variables filter";
const ARGUMENTS_INVALID: &str = "request arguments must be an object";
const THREAD_ID_INVALID: &str = "missing or invalid threadId";
const FRAME_ID_INVALID: &str = "missing or invalid frameId";
const LOCAL_VALUE_BLOCKER: &str = "canonical runtime value expansion facts";
const ERROR_UNSUPPORTED_REQUEST: DapError =
    DapError::new("dap.unsupportedRequest", STATUS_UNSUPPORTED_REQUEST);
const ERROR_ARGUMENTS_INVALID: DapError =
    DapError::new("dap.arguments.invalid", STATUS_INVALID_PARAMS);
const ERROR_LAUNCH_PROJECT_INVALID_PARAMS: DapError =
    DapError::new("dap.launchProject.invalid", STATUS_INVALID_PARAMS);
const ERROR_LAUNCH_ARGS_INVALID_TYPE: DapError =
    DapError::new("dap.launchArgs.invalidType", STATUS_INVALID_PARAMS);
const ERROR_LAUNCH_ENTRY_INVALID_TYPE: DapError =
    DapError::new("dap.launchEntry.invalidType", STATUS_INVALID_PARAMS);
const ERROR_LAUNCH_STOP_ON_ENTRY_INVALID_TYPE: DapError =
    DapError::new("dap.launchStopOnEntry.invalidType", STATUS_INVALID_PARAMS);
const ERROR_LAUNCH_ALREADY_CONFIGURED: DapError =
    DapError::new("dap.launch.alreadyConfigured", STATUS_INVALID_STATE);
const ERROR_BREAKPOINT_UNVERIFIED: DapError =
    DapError::blocked("dap.breakpoint.unverified", STOP_POINT_FACTS);
const ERROR_BREAKPOINT_EXPRESSION_BLOCKED: DapError = DapError::blocked(
    "dap.breakpoint.expressionBlocked",
    BREAKPOINT_EXPRESSION_FACTS,
);
const ERROR_BREAKPOINT_SOURCE_INVALID: DapError =
    DapError::new("dap.breakpoint.sourceInvalid", STATUS_INVALID_PARAMS);
const ERROR_BREAKPOINT_SOURCE_REFERENCE_UNSUPPORTED: DapError = DapError::new(
    "dap.breakpoint.sourceReferenceUnsupported",
    STATUS_UNSUPPORTED_REQUEST,
);
const ERROR_BREAKPOINTS_INVALID: DapError =
    DapError::new("dap.breakpoint.breakpointsInvalid", STATUS_INVALID_PARAMS);
const ERROR_BREAKPOINT_INVALID_LINE: DapError =
    DapError::new("dap.breakpoint.invalidLine", STATUS_INVALID_PARAMS);
const ERROR_BREAKPOINT_NO_STOP_POINT: DapError =
    DapError::new("dap.breakpoint.noStopPoint", STATUS_INVALID_PARAMS);
const ERROR_CONFIGURATION_NOT_READY: DapError =
    DapError::new("dap.launch.notConfigured", STATUS_INVALID_STATE);
const ERROR_RUN_INVALID: DapError = DapError::new("dap.run.invalid", STATUS_RUNTIME_ERROR);
const ERROR_DURABLE_DATA_BLOCKED: DapError =
    DapError::blocked("dap.durableData.blocked", DURABLE_WATCH_PATH_FACTS);
const ERROR_NOT_RUNNING: DapError = DapError::new("dap.notRunning", STATUS_INVALID_STATE);
const ERROR_NOT_STOPPED: DapError = DapError::new("dap.notStopped", STATUS_INVALID_STATE);
const ERROR_EVALUATE_BLOCKED: DapError =
    DapError::blocked("dap.evaluate.blocked", EXPRESSION_EVALUATE_FACTS);
const ERROR_EVALUATE_EXPRESSION_INVALID: DapError =
    DapError::new("dap.evaluate.expressionInvalid", STATUS_INVALID_PARAMS);
const ERROR_EVALUATE_CONTEXT_INVALID: DapError =
    DapError::new("dap.evaluate.contextInvalid", STATUS_INVALID_PARAMS);
const ERROR_HOVER_EVALUATE_BLOCKED: DapError =
    DapError::blocked("dap.hoverEvaluate.blocked", HOVER_EVALUATE_FACTS);
const ERROR_VARIABLES_REFERENCE_INVALID: DapError =
    DapError::new("dap.variables.referenceInvalid", STATUS_INVALID_PARAMS);
const ERROR_VARIABLES_PAGE_INVALID: DapError =
    DapError::new("dap.variables.pageInvalid", STATUS_INVALID_PARAMS);
const ERROR_VARIABLES_FILTER_INVALID: DapError =
    DapError::new("dap.variables.filterInvalid", STATUS_INVALID_PARAMS);
const ERROR_THREAD_ID_INVALID: DapError =
    DapError::new("dap.threadId.invalid", STATUS_INVALID_PARAMS);
const ERROR_FRAME_ID_INVALID: DapError =
    DapError::new("dap.frameId.invalid", STATUS_INVALID_PARAMS);

#[derive(Clone, Copy)]
struct DapError {
    code: &'static str,
    status: &'static str,
    blocked_on: Option<&'static str>,
}

impl DapError {
    const fn new(code: &'static str, status: &'static str) -> Self {
        Self {
            code,
            status,
            blocked_on: None,
        }
    }

    const fn blocked(code: &'static str, blocked_on: &'static str) -> Self {
        Self {
            code,
            status: STATUS_BLOCKED_ON_MARROW,
            blocked_on: Some(blocked_on),
        }
    }

    fn from_launch_error(error: &crate::project::LaunchError) -> Self {
        Self {
            code: error.code(),
            status: error.status(),
            blocked_on: error.blocked_on(),
        }
    }

    fn metadata(self) -> Json {
        let mut value = json!({
            "code": self.code,
            "status": self.status,
        });
        if let Some(blocked_on) = self.blocked_on {
            value["blockedOn"] = json!(blocked_on);
        }
        value
    }
}

#[derive(Clone, Copy)]
enum BreakpointBlock {
    StopPoint,
    Expression,
}

impl BreakpointBlock {
    fn message(self) -> &'static str {
        match self {
            Self::StopPoint => BREAKPOINT_VERIFICATION_BLOCKED,
            Self::Expression => BREAKPOINT_EXPRESSION_BLOCKED,
        }
    }

    fn contract(self) -> DapError {
        match self {
            Self::StopPoint => ERROR_BREAKPOINT_UNVERIFIED,
            Self::Expression => ERROR_BREAKPOINT_EXPRESSION_BLOCKED,
        }
    }
}

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
    /// The run-thread, present once launched and configurationDone-d.
    running: Option<Running>,
    /// Pending launch captured between `launch` and `configurationDone`.
    pending: Option<PendingLaunch>,
    stop_points: Option<StopPointIndex>,
    breakpoints: HashMap<PathBuf, Vec<RequestedBreakpoint>>,
    supports_variable_paging: bool,
    /// The dynamic reference registry for the current stop, cleared on resume.
    expandables: HashMap<i64, Expandable>,
    next_ref: i64,
    /// Set once the client asked to disconnect/terminate, so the loop exits.
    done: bool,
}

/// Launch state held until `configurationDone` starts the run.
struct PendingLaunch {
    project_dir: PathBuf,
    entry: Option<String>,
    analysis_identity: marrow_check::AnalysisIdentity,
    ephemeral_entry_identity: bool,
    invocation: EntryInvocation,
    stop_on_entry: bool,
}

#[derive(Clone)]
enum RequestedBreakpoint {
    Valid { line: u32, block: BreakpointBlock },
    InvalidLine,
}

enum BreakpointSource {
    Path(PathBuf),
    SourceReference,
}

#[derive(Clone, Default)]
struct StopPointIndex {
    runtime_files_by_canonical_file: HashMap<PathBuf, HashMap<u32, HashSet<PathBuf>>>,
}

impl StopPointIndex {
    fn from_runtime(program: &marrow_check::CheckedRuntimeProgram) -> Self {
        let mut index = Self::default();
        for point in program.stop_points() {
            let Some(path) = program.file_path(point.file_id) else {
                continue;
            };
            let runtime_path = path.to_path_buf();
            let canonical_path = normalize_existing_path(runtime_path.clone());
            index
                .runtime_files_by_canonical_file
                .entry(canonical_path)
                .or_default()
                .entry(point.span.line)
                .or_default()
                .insert(runtime_path);
        }
        index
    }

    fn verifies(&self, source: &Path, line: u32) -> bool {
        self.runtime_files(source, line)
            .is_some_and(|paths| !paths.is_empty())
    }

    fn runtime_files(&self, source: &Path, line: u32) -> Option<&HashSet<PathBuf>> {
        self.runtime_files_by_canonical_file
            .get(source)
            .and_then(|lines| lines.get(&line))
    }
}

struct DapInputError {
    message: String,
    contract: DapError,
}

impl DapInputError {
    fn new(message: impl Into<String>, contract: DapError) -> Self {
        Self {
            message: message.into(),
            contract,
        }
    }
}

struct VariablesInput {
    reference: i64,
    page: ChildPage,
    filter: VariablesFilter,
}

struct EvaluateInput<'a> {
    expression: &'a str,
    context: Option<&'a str>,
}

impl<W: Write> Session<W> {
    pub fn new(out: W) -> Self {
        Session {
            out,
            seq: 0,
            running: None,
            pending: None,
            stop_points: None,
            breakpoints: HashMap::new(),
            supports_variable_paging: false,
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

    fn respond_error(&mut self, request: &Json, message: impl Into<String>, error: DapError) {
        let seq = self.next_seq();
        let response = json!({
            "seq": seq,
            "type": "response",
            "request_seq": request.get("seq").cloned().unwrap_or(Json::Null),
            "success": false,
            "command": request.get("command").cloned().unwrap_or(Json::Null),
            "message": message.into(),
            "body": {
                "marrowError": error.metadata(),
            },
        });
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
        match command.as_str() {
            "initialize" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_initialize(request, &arguments);
            }
            "launch" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_launch(request, &arguments);
            }
            "setBreakpoints" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_set_breakpoints(request, &arguments);
            }
            "configurationDone" => {
                if self.arguments_or_reject(request).is_none() {
                    return;
                }
                self.on_configuration_done(request);
            }
            "threads" => {
                if self.arguments_or_reject(request).is_none() {
                    return;
                }
                self.on_threads(request);
            }
            "stackTrace" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_stack_trace(request, &arguments);
            }
            "scopes" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_scopes(request, &arguments);
            }
            "variables" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_variables(request, &arguments);
            }
            "continue" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_continue(request, &arguments);
            }
            "next" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_step(request, &arguments, StepKind::Over);
            }
            "stepIn" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_step(request, &arguments, StepKind::Into);
            }
            "stepOut" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_step(request, &arguments, StepKind::Out);
            }
            "pause" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_pause(request, &arguments);
            }
            "evaluate" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_evaluate(request, &arguments);
            }
            "disconnect" | "terminate" => {
                if self.arguments_or_reject(request).is_none() {
                    return;
                }
                self.on_disconnect(request);
            }
            other => self.respond_error(
                request,
                format!("unsupported request `{other}`"),
                ERROR_UNSUPPORTED_REQUEST,
            ),
        }
    }

    fn arguments_or_reject(&mut self, request: &Json) -> Option<Json> {
        match request_arguments(request) {
            Ok(arguments) => Some(arguments),
            Err(error) => {
                self.respond_error(request, error.message, error.contract);
                None
            }
        }
    }

    /// `initialize`: advertise the small capability set the debugger supports.
    fn on_initialize(&mut self, request: &Json, arguments: &Json) {
        self.supports_variable_paging = arguments
            .get("supportsVariablePaging")
            .and_then(Json::as_bool)
            .unwrap_or(false);
        self.respond(
            request,
            true,
            json!({
                "supportsConfigurationDoneRequest": true,
                "supportsTerminateRequest": true,
                "supportsEvaluateForHovers": false,
            }),
        );
    }

    /// `launch`: validate and stash the launch arguments. The run does not start
    /// until `configurationDone`.
    fn on_launch(&mut self, request: &Json, arguments: &Json) {
        if self.pending.is_some() || self.running.is_some() {
            self.respond_error(
                request,
                "launch is already configured or running",
                ERROR_LAUNCH_ALREADY_CONFIGURED,
            );
            return;
        }
        self.pending = None;
        self.stop_points = None;
        self.breakpoints.clear();
        let project = match parse_project(arguments) {
            Ok(project) => project,
            Err(error) => {
                self.respond_error(request, error.message, error.contract);
                return;
            }
        };
        let project_dir = normalize_existing_path(PathBuf::from(project));
        let entry = match parse_entry(arguments) {
            Ok(entry) => entry,
            Err(error) => {
                self.respond_error(request, error.message, error.contract);
                return;
            }
        };
        let args = match parse_launch_args(arguments.get("args")) {
            Ok(args) => args,
            Err(error) => {
                self.respond_error(request, error.message, error.contract);
                return;
            }
        };
        let stop_on_entry = match parse_stop_on_entry(arguments) {
            Ok(stop_on_entry) => stop_on_entry,
            Err(error) => {
                self.respond_error(request, error.message, error.contract);
                return;
            }
        };
        // Prepare now so a bad project fails launch. The run thread reopens the
        // project and requires the admitted source/config and entry identity to match.
        match crate::project::prepare(&project_dir, entry, &args) {
            Ok(launch) => {
                self.stop_points = Some(StopPointIndex::from_runtime(
                    launch.session.runtime_program(),
                ));
                self.pending = Some(PendingLaunch {
                    project_dir,
                    entry: entry.map(str::to_string),
                    analysis_identity: launch.analysis_identity,
                    ephemeral_entry_identity: launch.ephemeral_entry_identity,
                    invocation: launch.invocation,
                    stop_on_entry,
                });
                self.respond(request, true, json!({}));
                self.event("initialized", json!({}));
            }
            Err(error) => {
                let contract = DapError::from_launch_error(&error);
                self.respond_error(request, error.to_string(), contract);
            }
        }
    }

    /// `setBreakpoints`: replace the requested source breakpoints and verify
    /// plain line breakpoints when a launch has provided Marrow stop-point facts.
    fn on_set_breakpoints(&mut self, request: &Json, arguments: &Json) {
        let source = match breakpoint_source(arguments) {
            Ok(BreakpointSource::Path(source)) => source,
            Ok(BreakpointSource::SourceReference) => {
                self.respond_error(
                    request,
                    BREAKPOINT_SOURCE_REFERENCE_UNSUPPORTED,
                    ERROR_BREAKPOINT_SOURCE_REFERENCE_UNSUPPORTED,
                );
                return;
            }
            Err(error) => {
                self.respond_error(request, error.message, error.contract);
                return;
            }
        };

        let requested = match requested_breakpoints(arguments) {
            Ok(requested) => requested,
            Err(error) => {
                self.respond_error(request, error.message, error.contract);
                return;
            }
        };

        if self.stop_points.is_some() {
            if requested.is_empty() {
                self.breakpoints.remove(&source);
            } else {
                self.breakpoints.insert(source.clone(), requested.clone());
            }
            self.update_running_breakpoints();
        }
        let breakpoints = self.resolve_breakpoints(&source, &requested);
        self.respond(request, true, json!({ "breakpoints": breakpoints }));
    }

    /// Return breakpoint objects for the requested source lines.
    fn resolve_breakpoints(&self, source: &Path, requested: &[RequestedBreakpoint]) -> Vec<Json> {
        requested
            .iter()
            .map(|breakpoint| match breakpoint {
                RequestedBreakpoint::Valid { line, block } => {
                    self.resolved_breakpoint(source, *line, *block)
                }
                RequestedBreakpoint::InvalidLine => {
                    json!({
                        "verified": false,
                        "message": BREAKPOINT_INVALID_LINE,
                        "marrowContract": ERROR_BREAKPOINT_INVALID_LINE.metadata(),
                    })
                }
            })
            .collect()
    }

    fn resolved_breakpoint(&self, source: &Path, line: u32, block: BreakpointBlock) -> Json {
        if matches!(block, BreakpointBlock::Expression) {
            return unverified_breakpoint(line, block.message(), block.contract());
        }
        match &self.stop_points {
            Some(stop_points) if stop_points.verifies(source, line) => {
                json!({ "verified": true, "line": line })
            }
            Some(_) => unverified_breakpoint(
                line,
                BREAKPOINT_NO_STOP_POINT,
                ERROR_BREAKPOINT_NO_STOP_POINT,
            ),
            None => unverified_breakpoint(line, block.message(), block.contract()),
        }
    }

    fn armed_breakpoints(&self) -> ArmedBreakpoints {
        let mut armed = ArmedBreakpoints::default();
        let Some(stop_points) = &self.stop_points else {
            return armed;
        };
        for (source, requested) in &self.breakpoints {
            for breakpoint in requested {
                let RequestedBreakpoint::Valid {
                    line,
                    block: BreakpointBlock::StopPoint,
                } = breakpoint
                else {
                    continue;
                };
                let Some(runtime_files) = stop_points.runtime_files(source, *line) else {
                    continue;
                };
                for runtime_file in runtime_files {
                    armed.insert(runtime_file.clone(), *line);
                }
            }
        }
        armed
    }

    fn update_running_breakpoints(&mut self) {
        let armed = self.armed_breakpoints();
        if let Some(running) = self.running.as_ref()
            && running.stopped_at.is_some()
        {
            let _ = running.control.send(Control::SetBreakpoints(armed));
        }
    }

    /// `configurationDone`: start the run-thread with the captured launch. The
    /// run begins immediately; the first stop arrives as a `stopped` event the
    /// dispatcher pumps.
    fn on_configuration_done(&mut self, request: &Json) {
        let Some(pending) = self.pending.take() else {
            self.respond_error(
                request,
                "configurationDone before a launch",
                ERROR_CONFIGURATION_NOT_READY,
            );
            return;
        };
        let breakpoints = self.armed_breakpoints();
        match crate::run::spawn(
            pending.project_dir,
            pending.entry,
            pending.analysis_identity,
            pending.ephemeral_entry_identity,
            pending.invocation,
            pending.stop_on_entry,
            breakpoints,
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
                self.respond_error(request, error, ERROR_RUN_INVALID);
                self.terminate_session();
            }
        }
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
    fn on_stack_trace(&mut self, request: &Json, arguments: &Json) {
        if let Err(error) = parse_thread_id(arguments) {
            self.respond_error(request, error.message, error.contract);
            return;
        }
        let Some(stop) = self.running.as_ref().and_then(|r| r.stopped_at.as_ref()) else {
            self.respond(
                request,
                true,
                json!({ "stackFrames": [], "totalFrames": 0 }),
            );
            return;
        };
        let name = format!("depth {}", stop.depth);
        let mut frame = json!({
            "id": FRAME_ID,
            "name": name,
            "line": stop.line,
            "column": stop.column.max(1),
        });
        if let Some(path) = &stop.file {
            frame["source"] = json!({ "path": path });
        }
        self.respond(
            request,
            true,
            json!({ "stackFrames": [frame], "totalFrames": 1 }),
        );
    }

    /// `scopes`: Locals are available. Durable data is blocked until Marrow
    /// exposes canonical tooling facts.
    fn on_scopes(&mut self, request: &Json, arguments: &Json) {
        if let Err(error) = parse_frame_id(arguments) {
            self.respond_error(request, error.message, error.contract);
            return;
        }
        if self
            .running
            .as_ref()
            .and_then(|running| running.stopped_at.as_ref())
            .is_none()
        {
            self.respond_error(request, "not stopped", ERROR_NOT_STOPPED);
            return;
        }
        let scopes = vec![json!({
            "name": "Locals",
            "variablesReference": LOCALS_REF,
            "expensive": false,
        })];
        self.respond(request, true, json!({ "scopes": scopes }));
    }

    /// `variables`: list the children of a scope or an expandable local value.
    fn on_variables(&mut self, request: &Json, arguments: &Json) {
        let input = match parse_variables_input(arguments, self.supports_variable_paging) {
            Ok(input) => input,
            Err(error) => {
                self.respond_error(request, error.message, error.contract);
                return;
            }
        };
        if input.reference == DURABLE_REF {
            self.respond_error(
                request,
                RAW_DATA_INSPECTION_BLOCKED,
                ERROR_DURABLE_DATA_BLOCKED,
            );
            return;
        }
        let variables = match input.reference {
            LOCALS_REF => self.locals_variables(input.page, input.filter),
            other => {
                let children = match self.expandables.get(&other) {
                    Some(Expandable::Local(value)) => {
                        crate::variables::children(value, input.page, input.filter)
                    }
                    None => Vec::new(),
                };
                self.child_variables(children)
            }
        };
        self.respond(request, true, json!({ "variables": variables }));
    }

    /// The Locals scope contents: each local rendered, with a reference for any
    /// compound value the client can drill into.
    fn locals_variables(&mut self, page: ChildPage, filter: VariablesFilter) -> Vec<Json> {
        let Some(QueryResult::Locals(snapshot)) = self.query(Query::Locals { page, filter }) else {
            return Vec::new();
        };
        snapshot
            .entries
            .into_iter()
            .map(|local| {
                let counts = local
                    .expand
                    .as_ref()
                    .and_then(crate::variables::child_counts);
                let reference = local
                    .expand
                    .map(|value| self.alloc_ref(Expandable::Local(value)))
                    .unwrap_or(0);
                variable_json(
                    &local.name,
                    &local.value,
                    reference,
                    LOCAL_VALUE_BLOCKER,
                    counts,
                )
            })
            .collect()
    }

    fn child_variables(&mut self, children: Vec<crate::variables::Child>) -> Vec<Json> {
        children
            .into_iter()
            .map(|child| {
                let counts = child
                    .expand
                    .as_ref()
                    .and_then(crate::variables::child_counts);
                let reference = child
                    .expand
                    .map(|value| self.alloc_ref(Expandable::Local(value)))
                    .unwrap_or(0);
                variable_json(
                    &child.name,
                    &child.value,
                    reference,
                    LOCAL_VALUE_BLOCKER,
                    counts,
                )
            })
            .collect()
    }

    /// `continue`: resume until the run finishes or another supported stop occurs.
    fn on_continue(&mut self, request: &Json, arguments: &Json) {
        if let Err(error) = parse_thread_id(arguments) {
            self.respond_error(request, error.message, error.contract);
            return;
        }
        if self.running.is_none() {
            self.respond_error(request, "not running", ERROR_NOT_RUNNING);
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
    fn on_step(&mut self, request: &Json, arguments: &Json, kind: StepKind) {
        if let Err(error) = parse_thread_id(arguments) {
            self.respond_error(request, error.message, error.contract);
            return;
        }
        let Some(depth) = self
            .running
            .as_ref()
            .and_then(|r| r.stopped_at.as_ref())
            .map(|stop| stop.depth)
        else {
            self.respond_error(request, "not stopped", ERROR_NOT_STOPPED);
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
    fn on_pause(&mut self, request: &Json, arguments: &Json) {
        if let Err(error) = parse_thread_id(arguments) {
            self.respond_error(request, error.message, error.contract);
            return;
        }
        if self.running.is_none() {
            self.respond_error(request, "not running", ERROR_NOT_RUNNING);
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
        let input = match parse_evaluate_input(arguments) {
            Ok(input) => input,
            Err(error) => {
                self.respond_error(request, error.message, error.contract);
                return;
            }
        };
        if input.context == Some("hover") {
            self.respond_error(
                request,
                HOVER_EVALUATE_BLOCKED,
                ERROR_HOVER_EVALUATE_BLOCKED,
            );
            return;
        }

        if !input.expression.trim_start().starts_with('^') {
            self.respond_error(request, EXPRESSION_EVALUATE_BLOCKED, ERROR_EVALUATE_BLOCKED);
            return;
        }
        self.respond_error(
            request,
            RAW_DATA_INSPECTION_BLOCKED,
            ERROR_DURABLE_DATA_BLOCKED,
        );
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

fn unverified_breakpoint(line: u32, message: &str, contract: DapError) -> Json {
    json!({
        "verified": false,
        "line": line,
        "message": message,
        "marrowContract": contract.metadata(),
    })
}

/// Build a DAP `Variable` object. A zero reference marks a leaf; a non-zero one
/// lets the client expand it.
fn variable_json(
    name: &str,
    value: &str,
    reference: i64,
    blocked_on: &str,
    counts: Option<ChildCounts>,
) -> Json {
    let mut variable = json!({
        "name": name,
        "value": value,
        "presentationHint": debug_admin_presentation_hint(),
        "marrowContract": debug_admin_contract(blocked_on),
        "variablesReference": reference,
    });
    if let Some(counts) = counts {
        if let Some(named) = counts.named {
            variable["namedVariables"] = json!(named);
        }
        if let Some(indexed) = counts.indexed {
            variable["indexedVariables"] = json!(indexed);
        }
    }
    variable
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

fn parse_variables_input(
    arguments: &Json,
    supports_variable_paging: bool,
) -> Result<VariablesInput, DapInputError> {
    let reference = parse_variables_reference(arguments)?;
    let page = parse_child_page(arguments, supports_variable_paging)?;
    let filter = parse_variables_filter(arguments)?;
    Ok(VariablesInput {
        reference,
        page,
        filter,
    })
}

fn request_arguments(request: &Json) -> Result<Json, DapInputError> {
    match request.get("arguments") {
        None | Some(Json::Null) => Ok(json!({})),
        Some(value @ Json::Object(_)) => Ok(value.clone()),
        Some(_) => Err(DapInputError::new(
            ARGUMENTS_INVALID,
            ERROR_ARGUMENTS_INVALID,
        )),
    }
}

fn parse_child_page(
    arguments: &Json,
    supports_variable_paging: bool,
) -> Result<ChildPage, DapInputError> {
    let start = optional_usize(arguments, "start")?;
    let count = optional_usize(arguments, "count")?;
    if !supports_variable_paging {
        return Ok(ChildPage::default());
    }
    Ok(ChildPage::requested(start.unwrap_or(0), count))
}

fn optional_usize(arguments: &Json, key: &str) -> Result<Option<usize>, DapInputError> {
    match arguments.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Number(number)) => {
            let Some(value) = number.as_u64() else {
                return Err(variables_page_invalid());
            };
            let Ok(value) = usize::try_from(value) else {
                return Err(variables_page_invalid());
            };
            Ok(Some(value))
        }
        Some(_) => Err(variables_page_invalid()),
    }
}

fn parse_variables_filter(arguments: &Json) -> Result<VariablesFilter, DapInputError> {
    match arguments.get("filter") {
        None | Some(Json::Null) => Ok(VariablesFilter::All),
        Some(Json::String(value)) => match value.as_str() {
            "named" => Ok(VariablesFilter::Named),
            "indexed" => Ok(VariablesFilter::Indexed),
            _ => Err(variables_filter_invalid()),
        },
        Some(_) => Err(variables_filter_invalid()),
    }
}

fn variables_page_invalid() -> DapInputError {
    DapInputError::new(VARIABLES_PAGE_INVALID, ERROR_VARIABLES_PAGE_INVALID)
}

fn variables_filter_invalid() -> DapInputError {
    DapInputError::new(VARIABLES_FILTER_INVALID, ERROR_VARIABLES_FILTER_INVALID)
}

fn parse_variables_reference(arguments: &Json) -> Result<i64, DapInputError> {
    let Some(reference) = arguments
        .get("variablesReference")
        .and_then(Json::as_i64)
        .filter(|reference| *reference > 0)
    else {
        return Err(DapInputError::new(
            VARIABLES_REFERENCE_INVALID,
            ERROR_VARIABLES_REFERENCE_INVALID,
        ));
    };
    Ok(reference)
}

fn parse_thread_id(arguments: &Json) -> Result<(), DapInputError> {
    parse_singleton_id(
        arguments,
        "threadId",
        THREAD_ID,
        THREAD_ID_INVALID,
        ERROR_THREAD_ID_INVALID,
    )
}

fn parse_frame_id(arguments: &Json) -> Result<(), DapInputError> {
    parse_singleton_id(
        arguments,
        "frameId",
        FRAME_ID,
        FRAME_ID_INVALID,
        ERROR_FRAME_ID_INVALID,
    )
}

fn parse_singleton_id(
    arguments: &Json,
    field: &'static str,
    expected: i64,
    message: &'static str,
    contract: DapError,
) -> Result<(), DapInputError> {
    match arguments.get(field).and_then(Json::as_i64) {
        Some(value) if value == expected => Ok(()),
        _ => Err(DapInputError::new(message, contract)),
    }
}

fn breakpoint_source(arguments: &Json) -> Result<BreakpointSource, DapInputError> {
    let Some(source) = arguments.get("source").and_then(Json::as_object) else {
        return Err(DapInputError::new(
            BREAKPOINT_SOURCE_INVALID,
            ERROR_BREAKPOINT_SOURCE_INVALID,
        ));
    };
    if source
        .get("sourceReference")
        .and_then(Json::as_u64)
        .is_some_and(|reference| reference > 0)
    {
        return Ok(BreakpointSource::SourceReference);
    }
    if let Some(path) = source.get("path") {
        return path
            .as_str()
            .map(|path| BreakpointSource::Path(normalize_existing_path(PathBuf::from(path))))
            .ok_or_else(|| {
                DapInputError::new(BREAKPOINT_SOURCE_INVALID, ERROR_BREAKPOINT_SOURCE_INVALID)
            });
    }
    Err(DapInputError::new(
        BREAKPOINT_SOURCE_INVALID,
        ERROR_BREAKPOINT_SOURCE_INVALID,
    ))
}

fn requested_breakpoints(arguments: &Json) -> Result<Vec<RequestedBreakpoint>, DapInputError> {
    let Some(points) = arguments.get("breakpoints") else {
        return Ok(Vec::new());
    };
    let Some(points) = points.as_array() else {
        return Err(DapInputError::new(
            BREAKPOINTS_INVALID,
            ERROR_BREAKPOINTS_INVALID,
        ));
    };
    Ok(points.iter().map(requested_breakpoint).collect())
}

fn parse_evaluate_input(arguments: &Json) -> Result<EvaluateInput<'_>, DapInputError> {
    let Some(expression) = arguments.get("expression").and_then(Json::as_str) else {
        return Err(DapInputError::new(
            EVALUATE_EXPRESSION_INVALID,
            ERROR_EVALUATE_EXPRESSION_INVALID,
        ));
    };
    let context = match arguments.get("context") {
        Some(context) => Some(context.as_str().ok_or_else(|| {
            DapInputError::new(EVALUATE_CONTEXT_INVALID, ERROR_EVALUATE_CONTEXT_INVALID)
        })?),
        None => None,
    };
    Ok(EvaluateInput {
        expression,
        context,
    })
}

fn requested_breakpoint(point: &Json) -> RequestedBreakpoint {
    let Some(line) = point
        .get("line")
        .and_then(Json::as_u64)
        .and_then(|line| u32::try_from(line).ok())
        .filter(|line| *line > 0)
    else {
        return RequestedBreakpoint::InvalidLine;
    };
    RequestedBreakpoint::Valid {
        line,
        block: breakpoint_block(point),
    }
}

fn breakpoint_block(point: &Json) -> BreakpointBlock {
    for field in ["condition", "hitCondition", "logMessage"] {
        if point
            .get(field)
            .and_then(Json::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        {
            return BreakpointBlock::Expression;
        }
    }
    BreakpointBlock::StopPoint
}

fn parse_project(arguments: &Json) -> Result<&str, DapInputError> {
    match arguments.get("project") {
        Some(Json::String(project)) if !project.trim().is_empty() => Ok(project),
        _ => Err(DapInputError::new(
            "launch needs a non-empty `project` directory",
            ERROR_LAUNCH_PROJECT_INVALID_PARAMS,
        )),
    }
}

fn normalize_existing_path(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn parse_entry(arguments: &Json) -> Result<Option<&str>, DapInputError> {
    match arguments.get("entry") {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(entry)) => Ok(Some(entry)),
        Some(_) => Err(DapInputError::new(
            "`entry` must be a string when present",
            ERROR_LAUNCH_ENTRY_INVALID_TYPE,
        )),
    }
}

fn parse_stop_on_entry(arguments: &Json) -> Result<bool, DapInputError> {
    match arguments.get("stopOnEntry") {
        None | Some(Json::Null) => Ok(false),
        Some(Json::Bool(stop_on_entry)) => Ok(*stop_on_entry),
        Some(_) => Err(DapInputError::new(
            "`stopOnEntry` must be a boolean",
            ERROR_LAUNCH_STOP_ON_ENTRY_INVALID_TYPE,
        )),
    }
}

/// Parse the optional launch `args` array into Marrow's typed entry DTOs.
fn parse_launch_args(args: Option<&Json>) -> Result<Vec<EntryArgument>, DapInputError> {
    let Some(array) = args else {
        return Ok(Vec::new());
    };
    let Some(items) = array.as_array() else {
        return Err(DapInputError::new(
            "`args` must be an array",
            ERROR_LAUNCH_ARGS_INVALID_TYPE,
        ));
    };
    marrow_run::entry_arguments_from_json(items)
        .map_err(|error| DapInputError::new(error.message(), ERROR_LAUNCH_ARGS_INVALID_TYPE))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            breakpoint["marrowContract"]["code"], "dap.breakpoint.unverified",
            "{response}"
        );
        assert_eq!(
            breakpoint["marrowContract"]["status"], "blocked-on-marrow",
            "{response}"
        );
        assert_eq!(
            breakpoint["marrowContract"]["blockedOn"], "canonical stop-point facts",
            "{response}"
        );
    }
}
