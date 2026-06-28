//! The DAP session: the protocol-thread state machine.
//!
//! It owns the sequence counter, the variable-reference registry, and the handle
//! to the run-thread. Each client request is handled here, on the protocol
//! thread; inspection requests at a stop are forwarded to the parked run-thread
//! as [`Query`] messages and answered from owned data. The session never borrows
//! the runtime frame — that lives only on the run-thread.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

use marrow_check::{AnalysisSnapshot, CheckedDebugExpression, MarrowType, ScalarType, tooling};
use marrow_lsp_core::execution::{debug_data_boundary_json, execution_boundary_json};
use marrow_run::{DebugValue, EntryArgument, EntryInvocation, SourceAnalysisAdmission};
use marrow_syntax::SourceSpan;
use serde_json::{Value as Json, json};

use crate::debugger::{
    ArmedBreakpoint, ArmedBreakpoints, CheckedLogMessage, Control, LogMessagePart,
    ParsedLogMessage, Query, QueryResult, RunEvent, StopInfo,
};
use crate::protocol::write_message;
use crate::step::Resume;
use crate::variables::{ChildCounts, ChildPage, VariablesFilter};

/// The variable reference for the Locals scope at a stop. Fixed, since there is
/// one Locals scope per frame.
const LOCALS_REF: i64 = 1;
/// The variable reference for the Durable Data scope at a stop.
const DURABLE_REF: i64 = 2;
/// The first dynamically-allocated reference for expandable locals.
const FIRST_DYNAMIC_REF: i64 = 1000;
/// The single thread id the debugger exposes — the run is single-threaded.
const THREAD_ID: i64 = 1;
/// The single stack frame id exposed while stopped. It is a frame identity, not
/// the thread identity, even while both singleton ids share the same value.
const FRAME_ID: i64 = 1;
const STATUS_INVALID_PARAMS: &str = "invalid-params";
const STATUS_INVALID_STATE: &str = "invalid-state";
const STATUS_RUNTIME_ERROR: &str = "runtime-error";
const STATUS_UNSUPPORTED_REQUEST: &str = "unsupported-request";
const STATUS_BLOCKED_ON_MARROW: &str = "blocked-on-marrow";
const ATTACH_BLOCKED: &str = "DAP attach requires Marrow serve/attach execution boundary facts";
const ATTACH_BLOCKED_ON: &str = "serve/attach execution boundary facts";
const BREAKPOINT_CONFIGURATION_NOT_READY: &str =
    "breakpoint verification requires launch configuration";
const BREAKPOINT_CONDITION_INVALID: &str = "invalid conditional breakpoint expression";
const BREAKPOINT_HIT_CONDITION_INVALID: &str = "hitCondition must be a positive integer string";
const BREAKPOINT_LOG_MESSAGE_INVALID: &str = "invalid logMessage expression";
const BREAKPOINT_SOURCE_INVALID: &str = "missing or invalid breakpoint source";
const BREAKPOINT_SOURCE_REFERENCE_INVALID: &str = "unknown DAP sourceReference";
const BREAKPOINTS_INVALID: &str = "`breakpoints` must be an array";
const BREAKPOINT_INVALID_LINE: &str = "missing or invalid breakpoint line";
const BREAKPOINT_NO_STOP_POINT: &str = "no executable Marrow stop point at this source line";
const EVALUATE_EXPRESSION_INVALID: &str = "missing or invalid evaluate expression";
const EVALUATE_CONTEXT_INVALID: &str = "invalid evaluate context";
const SOURCE_REFERENCE_INVALID: &str = "missing or unknown DAP sourceReference";
const VARIABLES_REFERENCE_INVALID: &str = "missing or invalid variablesReference";
const VARIABLES_PAGE_INVALID: &str = "invalid variables paging arguments";
const VARIABLES_FILTER_INVALID: &str = "invalid variables filter";
const ARGUMENTS_INVALID: &str = "request arguments must be an object";
const THREAD_ID_INVALID: &str = "missing or invalid threadId";
const FRAME_ID_INVALID: &str = "missing or invalid frameId";
const ERROR_UNSUPPORTED_REQUEST: DapError =
    DapError::new("dap.unsupportedRequest", STATUS_UNSUPPORTED_REQUEST);
const ERROR_ATTACH_BLOCKED: DapError = DapError::blocked("dap.attach.blocked", ATTACH_BLOCKED_ON);
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
    DapError::new("dap.breakpoint.unverified", STATUS_INVALID_STATE);
const ERROR_BREAKPOINT_CONDITION_INVALID: DapError =
    DapError::new("dap.breakpoint.conditionInvalid", STATUS_INVALID_PARAMS);
const ERROR_BREAKPOINT_HIT_CONDITION_INVALID: DapError =
    DapError::new("dap.breakpoint.hitConditionInvalid", STATUS_INVALID_PARAMS);
const ERROR_BREAKPOINT_LOG_MESSAGE_INVALID: DapError =
    DapError::new("dap.breakpoint.logMessageInvalid", STATUS_INVALID_PARAMS);
const ERROR_BREAKPOINT_SOURCE_INVALID: DapError =
    DapError::new("dap.breakpoint.sourceInvalid", STATUS_INVALID_PARAMS);
const ERROR_BREAKPOINT_SOURCE_REFERENCE_INVALID: DapError = DapError::new(
    "dap.breakpoint.sourceReferenceInvalid",
    STATUS_INVALID_PARAMS,
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
const ERROR_NOT_RUNNING: DapError = DapError::new("dap.notRunning", STATUS_INVALID_STATE);
const ERROR_NOT_STOPPED: DapError = DapError::new("dap.notStopped", STATUS_INVALID_STATE);
const ERROR_EVALUATE_EXPRESSION_INVALID: DapError =
    DapError::new("dap.evaluate.expressionInvalid", STATUS_INVALID_PARAMS);
const ERROR_EVALUATE_CONTEXT_INVALID: DapError =
    DapError::new("dap.evaluate.contextInvalid", STATUS_INVALID_PARAMS);
const ERROR_EVALUATE_RUNTIME: DapError =
    DapError::new("dap.evaluate.runtimeError", STATUS_RUNTIME_ERROR);
const ERROR_SOURCE_REFERENCE_INVALID: DapError =
    DapError::new("dap.source.sourceReferenceInvalid", STATUS_INVALID_PARAMS);
const ERROR_VARIABLES_REFERENCE_INVALID: DapError =
    DapError::new("dap.variables.referenceInvalid", STATUS_INVALID_PARAMS);
const ERROR_VARIABLES_PAGE_INVALID: DapError =
    DapError::new("dap.variables.pageInvalid", STATUS_INVALID_PARAMS);
const ERROR_VARIABLES_FILTER_INVALID: DapError =
    DapError::new("dap.variables.filterInvalid", STATUS_INVALID_PARAMS);
const ERROR_VARIABLES_RUNTIME: DapError =
    DapError::new("dap.variables.runtimeError", STATUS_RUNTIME_ERROR);
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

#[derive(Clone)]
enum BreakpointRequest {
    Valid(BreakpointSpec),
    InvalidCondition,
    InvalidHitCondition,
    InvalidLogMessage,
}

#[derive(Clone, Default)]
struct BreakpointSpec {
    condition: Option<String>,
    hit_target: Option<u64>,
    log_message: Option<ParsedLogMessage>,
}

struct CheckedBreakpointSpec {
    condition: Option<CheckedDebugExpression>,
    log_message: Option<CheckedLogMessage>,
}

/// What a dynamic variable reference expands to. Resolved on the run-thread.
#[derive(Clone)]
enum Expandable {
    Value {
        value: DebugValue,
        store_snapshot: Option<tooling::DataSnapshotStamp>,
    },
    DataPath(Vec<tooling::DataPathSegment>),
}

/// The live run-thread plus the channels to drive it.
struct Running {
    handle: JoinHandle<Option<crate::run::Outcome>>,
    control: Sender<Control>,
    events: Receiver<RunEvent>,
    terminate: Arc<AtomicBool>,
    execution_boundary: Json,
    /// The most recent stop, kept so `stackTrace`/`scopes` answer without
    /// re-querying. `None` while the run is executing (not parked).
    stopped_at: Option<StopInfo>,
    pending_resume: Option<Resume>,
    terminating: bool,
}

/// The protocol-thread session.
pub struct Session<W: Write> {
    out: W,
    seq: i64,
    /// The run-thread, present once launched and configurationDone-d.
    running: Option<Running>,
    /// Pending launch captured between `launch` and `configurationDone`.
    pending: Option<PendingLaunch>,
    analysis: Option<AnalysisSnapshot>,
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
    analysis_generation: marrow_check::AnalysisGeneration,
    source_analysis_admission: Option<SourceAnalysisAdmission>,
    invocation: EntryInvocation,
    stop_on_entry: bool,
}

#[derive(Clone)]
enum RequestedBreakpoint {
    Valid {
        line: u32,
        request: BreakpointRequest,
    },
    InvalidLine,
}

enum BreakpointSource {
    Path(PathBuf),
    SourceReference(i64),
}

#[derive(Clone, Default)]
struct StopPointIndex {
    stops_by_canonical_file: HashMap<PathBuf, HashMap<u32, Vec<RuntimeStop>>>,
    sources_by_reference: HashMap<i64, DapSourceFile>,
    references_by_canonical_file: HashMap<PathBuf, i64>,
}

#[derive(Clone)]
struct RuntimeStop {
    file: PathBuf,
    span: SourceSpan,
}

#[derive(Clone)]
struct DapSourceFile {
    canonical_path: PathBuf,
    source: String,
}

impl StopPointIndex {
    fn from_runtime(
        program: &marrow_check::CheckedRuntimeProgram,
        analysis: &AnalysisSnapshot,
    ) -> Self {
        let source_by_canonical_file: HashMap<PathBuf, &str> = analysis
            .files
            .iter()
            .map(|file| {
                (
                    normalize_existing_path(file.path.clone()),
                    file.source.as_str(),
                )
            })
            .collect();
        let mut index = Self::default();
        for point in program.stop_points() {
            let Some(path) = program.file_path(point.file_id) else {
                continue;
            };
            let runtime_path = path.to_path_buf();
            let canonical_path = normalize_existing_path(runtime_path.clone());
            if let Some(source) = source_by_canonical_file.get(&canonical_path) {
                index.register_source(canonical_path.clone(), source);
            }
            index
                .stops_by_canonical_file
                .entry(canonical_path)
                .or_default()
                .entry(point.span.line)
                .or_default()
                .push(RuntimeStop {
                    file: runtime_path,
                    span: point.span,
                });
        }
        index
    }

    fn register_source(&mut self, canonical_path: PathBuf, source: &str) {
        if self
            .references_by_canonical_file
            .contains_key(&canonical_path)
        {
            return;
        }
        let reference = i64::try_from(self.sources_by_reference.len() + 1)
            .expect("DAP sourceReference count fits in i64");
        self.references_by_canonical_file
            .insert(canonical_path.clone(), reference);
        self.sources_by_reference.insert(
            reference,
            DapSourceFile {
                canonical_path,
                source: source.to_string(),
            },
        );
    }

    fn stops(&self, source: &Path, line: u32) -> Option<&[RuntimeStop]> {
        self.stops_by_canonical_file
            .get(source)
            .and_then(|lines| lines.get(&line))
            .map(Vec::as_slice)
    }

    fn source_reference(&self, source: &Path) -> Option<i64> {
        self.references_by_canonical_file.get(source).copied()
    }

    fn source_file(&self, reference: i64) -> Option<&DapSourceFile> {
        self.sources_by_reference.get(&reference)
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
}

enum RunPump {
    Continue,
    ParkedOrDone,
}

impl<W: Write> Session<W> {
    pub fn new(out: W) -> Self {
        Session {
            out,
            seq: 0,
            running: None,
            pending: None,
            analysis: None,
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
            "attach" => {
                if self.arguments_or_reject(request).is_none() {
                    return;
                }
                self.on_attach(request);
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
            "source" => {
                let Some(arguments) = self.arguments_or_reject(request) else {
                    return;
                };
                self.on_source(request, &arguments);
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
            "disconnect" => {
                if self.arguments_or_reject(request).is_none() {
                    return;
                }
                self.on_disconnect(request);
            }
            "terminate" => {
                if self.arguments_or_reject(request).is_none() {
                    return;
                }
                self.on_terminate(request);
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
                "supportsConditionalBreakpoints": true,
                "supportsHitConditionalBreakpoints": true,
                "supportsLogPoints": true,
                "supportsEvaluateForHovers": true,
            }),
        );
    }

    fn on_attach(&mut self, request: &Json) {
        self.respond_error(request, ATTACH_BLOCKED, ERROR_ATTACH_BLOCKED);
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
        self.clear_launch_configuration();
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
        // Prepare now so a bad project fails launch. The run thread reopens
        // through Marrow's source-analysis admission so debug-expression facts
        // stay aligned with runtime frames.
        match crate::project::prepare(&project_dir, entry, &args) {
            Ok(launch) => {
                let execution_boundary = execution_boundary_json(&launch.session);
                self.stop_points = Some(StopPointIndex::from_runtime(
                    launch.session.runtime_program(),
                    &launch.analysis_snapshot,
                ));
                self.analysis = Some(launch.analysis_snapshot);
                self.pending = Some(PendingLaunch {
                    project_dir,
                    entry: entry.map(str::to_string),
                    analysis_generation: launch.analysis_generation,
                    source_analysis_admission: launch.source_analysis_admission,
                    invocation: launch.invocation,
                    stop_on_entry,
                });
                self.respond(
                    request,
                    true,
                    json!({ "marrowDebug": { "previewExecutionBoundary": execution_boundary } }),
                );
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
            Ok(BreakpointSource::SourceReference(reference)) => match self
                .stop_points
                .as_ref()
                .and_then(|stops| stops.source_file(reference))
                .map(|source| source.canonical_path.clone())
            {
                Some(source) => source,
                None => {
                    self.respond_error(
                        request,
                        BREAKPOINT_SOURCE_REFERENCE_INVALID,
                        ERROR_BREAKPOINT_SOURCE_REFERENCE_INVALID,
                    );
                    return;
                }
            },
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

        if self
            .running
            .as_ref()
            .is_some_and(|running| running.stopped_at.is_none())
        {
            self.respond_error(request, "not stopped", ERROR_NOT_STOPPED);
            return;
        }

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
                RequestedBreakpoint::Valid { line, request } => {
                    self.resolved_breakpoint(source, *line, request)
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

    fn resolved_breakpoint(&self, source: &Path, line: u32, request: &BreakpointRequest) -> Json {
        match &self.stop_points {
            Some(stop_points) => {
                self.resolved_launched_breakpoint(source, line, request, stop_points)
            }
            None => match request {
                BreakpointRequest::Valid(_) => unverified_breakpoint(
                    line,
                    BREAKPOINT_CONFIGURATION_NOT_READY,
                    ERROR_BREAKPOINT_UNVERIFIED,
                ),
                BreakpointRequest::InvalidCondition => unverified_breakpoint(
                    line,
                    BREAKPOINT_CONDITION_INVALID,
                    ERROR_BREAKPOINT_CONDITION_INVALID,
                ),
                BreakpointRequest::InvalidHitCondition => unverified_breakpoint(
                    line,
                    BREAKPOINT_HIT_CONDITION_INVALID,
                    ERROR_BREAKPOINT_HIT_CONDITION_INVALID,
                ),
                BreakpointRequest::InvalidLogMessage => unverified_breakpoint(
                    line,
                    BREAKPOINT_LOG_MESSAGE_INVALID,
                    ERROR_BREAKPOINT_LOG_MESSAGE_INVALID,
                ),
            },
        }
    }

    fn resolved_launched_breakpoint(
        &self,
        source: &Path,
        line: u32,
        request: &BreakpointRequest,
        stop_points: &StopPointIndex,
    ) -> Json {
        let Some(stops) = stop_points.stops(source, line) else {
            return unverified_breakpoint(
                line,
                BREAKPOINT_NO_STOP_POINT,
                ERROR_BREAKPOINT_NO_STOP_POINT,
            );
        };
        match request {
            BreakpointRequest::Valid(spec) => match self.checked_breakpoint_spec(&stops[0], spec) {
                Ok(_) => json!({ "verified": true, "line": line }),
                Err(error) => unverified_breakpoint(line, &error.message, error.contract),
            },
            BreakpointRequest::InvalidCondition => unverified_breakpoint(
                line,
                BREAKPOINT_CONDITION_INVALID,
                ERROR_BREAKPOINT_CONDITION_INVALID,
            ),
            BreakpointRequest::InvalidHitCondition => unverified_breakpoint(
                line,
                BREAKPOINT_HIT_CONDITION_INVALID,
                ERROR_BREAKPOINT_HIT_CONDITION_INVALID,
            ),
            BreakpointRequest::InvalidLogMessage => unverified_breakpoint(
                line,
                BREAKPOINT_LOG_MESSAGE_INVALID,
                ERROR_BREAKPOINT_LOG_MESSAGE_INVALID,
            ),
        }
    }

    fn checked_breakpoint_spec(
        &self,
        stop: &RuntimeStop,
        spec: &BreakpointSpec,
    ) -> Result<CheckedBreakpointSpec, DapInputError> {
        let condition = spec
            .condition
            .as_deref()
            .map(|condition| self.checked_condition(stop, condition))
            .transpose()?;
        let log_message = spec
            .log_message
            .as_ref()
            .map(|message| self.checked_log_message(stop, message))
            .transpose()?;
        Ok(CheckedBreakpointSpec {
            condition,
            log_message,
        })
    }

    fn checked_condition(
        &self,
        stop: &RuntimeStop,
        source: &str,
    ) -> Result<CheckedDebugExpression, DapInputError> {
        let Some(analysis) = &self.analysis else {
            return Err(DapInputError::new(
                BREAKPOINT_CONFIGURATION_NOT_READY,
                ERROR_BREAKPOINT_UNVERIFIED,
            ));
        };
        let expression = analysis
            .checked_debug_expression(&stop.file, stop.span, source)
            .map_err(|diagnostics| {
                DapInputError::new(
                    diagnostic_message(BREAKPOINT_CONDITION_INVALID, &diagnostics),
                    ERROR_BREAKPOINT_CONDITION_INVALID,
                )
            })?;
        if !matches!(expression.ty(), MarrowType::Primitive(ScalarType::Bool)) {
            return Err(DapInputError::new(
                BREAKPOINT_CONDITION_INVALID,
                ERROR_BREAKPOINT_CONDITION_INVALID,
            ));
        }
        Ok(expression)
    }

    fn checked_log_message(
        &self,
        stop: &RuntimeStop,
        template: &ParsedLogMessage,
    ) -> Result<CheckedLogMessage, DapInputError> {
        let mut parts = Vec::with_capacity(template.parts().len());
        for part in template.parts() {
            match part {
                LogMessagePart::Literal(value) => {
                    parts.push(LogMessagePart::Literal(value.clone()));
                }
                LogMessagePart::Expression(source) => {
                    let Some(analysis) = &self.analysis else {
                        return Err(DapInputError::new(
                            BREAKPOINT_CONFIGURATION_NOT_READY,
                            ERROR_BREAKPOINT_UNVERIFIED,
                        ));
                    };
                    let expression = analysis
                        .checked_debug_expression(&stop.file, stop.span, source)
                        .map_err(|diagnostics| {
                            DapInputError::new(
                                diagnostic_message(BREAKPOINT_LOG_MESSAGE_INVALID, &diagnostics),
                                ERROR_BREAKPOINT_LOG_MESSAGE_INVALID,
                            )
                        })?;
                    parts.push(LogMessagePart::Expression(Box::new(expression)));
                }
            }
        }
        Ok(CheckedLogMessage::new(parts))
    }

    fn armed_breakpoints(&self) -> ArmedBreakpoints {
        let mut armed = ArmedBreakpoints::default();
        let Some(stop_points) = &self.stop_points else {
            return armed;
        };
        for (source, requested) in &self.breakpoints {
            for breakpoint in requested {
                let RequestedBreakpoint::Valid { line, request } = breakpoint else {
                    continue;
                };
                let Some(stops) = stop_points.stops(source, *line) else {
                    continue;
                };
                match request {
                    BreakpointRequest::Valid(spec) => {
                        for stop in stops {
                            if let Ok(checked) = self.checked_breakpoint_spec(stop, spec) {
                                armed.insert(
                                    stop.file.clone(),
                                    ArmedBreakpoint::new(
                                        *line,
                                        checked.condition,
                                        spec.hit_target,
                                        checked.log_message,
                                    ),
                                );
                            }
                        }
                    }
                    BreakpointRequest::InvalidCondition
                    | BreakpointRequest::InvalidHitCondition
                    | BreakpointRequest::InvalidLogMessage => {}
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
            pending.analysis_generation,
            pending.source_analysis_admission,
            pending.invocation,
            pending.stop_on_entry,
            breakpoints,
        ) {
            Ok(running) => {
                let execution_boundary = running.execution_boundary.clone();
                self.running = Some(Running {
                    handle: running.handle,
                    control: running.control,
                    events: running.events,
                    terminate: running.terminate,
                    execution_boundary: execution_boundary.clone(),
                    stopped_at: None,
                    pending_resume: None,
                    terminating: false,
                });
                self.respond(
                    request,
                    true,
                    json!({ "marrowDebug": { "executionBoundary": execution_boundary } }),
                );
            }
            Err(crate::run::SpawnError::Launch(error)) => {
                let contract = DapError::from_launch_error(&error);
                self.clear_launch_configuration();
                self.respond_error(request, error.to_string(), contract);
            }
            Err(crate::run::SpawnError::Thread(error)) => {
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
            let mut source = json!({ "path": path });
            if let Some(reference) = self
                .stop_points
                .as_ref()
                .and_then(|stops| stops.source_reference(&normalize_existing_path(path.clone())))
            {
                source["sourceReference"] = json!(reference);
                if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                    source["name"] = json!(name);
                }
            }
            frame["source"] = source;
        }
        self.respond(
            request,
            true,
            json!({ "stackFrames": [frame], "totalFrames": 1 }),
        );
    }

    /// `source`: return the launch-time source text behind a DAP sourceReference.
    fn on_source(&mut self, request: &Json, arguments: &Json) {
        let reference = match parse_source_reference(arguments) {
            Ok(reference) => reference,
            Err(error) => {
                self.respond_error(request, error.message, error.contract);
                return;
            }
        };
        let Some(source) = self
            .stop_points
            .as_ref()
            .and_then(|stops| stops.source_file(reference))
        else {
            self.respond_error(
                request,
                SOURCE_REFERENCE_INVALID,
                ERROR_SOURCE_REFERENCE_INVALID,
            );
            return;
        };
        self.respond(
            request,
            true,
            json!({
                "content": source.source.as_str(),
                "mimeType": "text/x-marrow",
            }),
        );
    }

    /// `scopes`: Locals and durable saved data are both served by Marrow runtime
    /// debug facts from the parked frame.
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
        let scopes = vec![
            json!({
                "name": "Locals",
                "variablesReference": LOCALS_REF,
                "expensive": false,
            }),
            json!({
                "name": "Durable Data",
                "variablesReference": DURABLE_REF,
                "expensive": false,
            }),
        ];
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
        let body = match input.reference {
            LOCALS_REF => Ok(self.locals_variables(input.page, input.filter)),
            DURABLE_REF => self.data_variables(Vec::new(), input.page),
            other => self.expandable_variables(other, input.page, input.filter),
        };
        let body = match body {
            Ok(body) => body,
            Err(error) => {
                self.respond_error(
                    request,
                    format!("{}: {}", error.code, error.message),
                    ERROR_VARIABLES_RUNTIME,
                );
                return;
            }
        };
        self.respond(request, true, body);
    }

    /// The Locals scope contents: each local rendered, with a reference for any
    /// compound value the client can drill into.
    fn locals_variables(&mut self, page: ChildPage, filter: VariablesFilter) -> Json {
        let Some(QueryResult::Locals(snapshot)) = self.query(Query::Locals { page, filter }) else {
            return variables_body(Vec::new());
        };
        let visible_local_count = snapshot.visible_local_count;
        let locals_truncated = snapshot.locals_truncated;
        let variables = snapshot
            .entries
            .into_iter()
            .map(|local| {
                let counts = local
                    .expand
                    .as_ref()
                    .and_then(crate::variables::child_counts);
                let reference = local
                    .expand
                    .map(|value| {
                        self.alloc_ref(Expandable::Value {
                            value,
                            store_snapshot: None,
                        })
                    })
                    .unwrap_or(0);
                variable_json(
                    &local.name,
                    &local.value,
                    reference,
                    counts,
                    local.children_truncated,
                )
            })
            .collect();
        let mut body = variables_body(variables);
        body["marrowDebug"] = json!({
            "visibleLocalCount": visible_local_count,
            "localsTruncated": locals_truncated,
        });
        body
    }

    fn expandable_variables(
        &mut self,
        reference: i64,
        page: ChildPage,
        filter: VariablesFilter,
    ) -> Result<Json, crate::debugger::RuntimeErrorDetail> {
        let Some(expandable) = self.expandables.get(&reference).cloned() else {
            return Ok(variables_body(Vec::new()));
        };
        let (value, store_snapshot) = match expandable {
            Expandable::Value {
                value,
                store_snapshot,
            } => (value, store_snapshot),
            Expandable::DataPath(path) => return self.data_variables(path, page),
        };
        let children_truncated = value.children_truncated();
        let children = crate::variables::children(&value, page, filter);
        let mut body = variables_body(self.child_variables(children, store_snapshot.as_ref()));
        body["marrowDebug"] = json!({
            "childrenTruncated": children_truncated,
        });
        if let Some(store_snapshot) = store_snapshot {
            let store_snapshot =
                marrow_lsp_core::data_explorer::data_generation_stamp_to_json(&store_snapshot);
            self.attach_debug_data_boundary(&mut body, store_snapshot);
        }
        Ok(body)
    }

    fn data_variables(
        &mut self,
        segments: Vec<tooling::DataPathSegment>,
        page: ChildPage,
    ) -> Result<Json, crate::debugger::RuntimeErrorDetail> {
        let Some(QueryResult::DataChildren(snapshot)) =
            self.query(Query::DataChildren { segments, page })
        else {
            return Ok(variables_body(Vec::new()));
        };
        let snapshot = snapshot?;
        let children_truncated = snapshot.children_truncated;
        let store_snapshot =
            marrow_lsp_core::data_explorer::data_generation_stamp_to_json(&snapshot.store_snapshot);
        let variables = snapshot
            .entries
            .into_iter()
            .map(|entry| {
                let reference = if entry.expandable {
                    self.alloc_ref(Expandable::DataPath(entry.path))
                } else {
                    0
                };
                variable_json(
                    &entry.name,
                    &entry.value,
                    reference,
                    None,
                    entry.children_truncated,
                )
            })
            .collect();
        let mut body = variables_body(variables);
        body["marrowDebug"] = json!({
            "childrenTruncated": children_truncated,
        });
        self.attach_debug_data_boundary(&mut body, store_snapshot);
        Ok(body)
    }

    fn attach_debug_data_boundary(&self, body: &mut Json, store_snapshot: Json) {
        body["marrowDebug"]["storeSnapshot"] = store_snapshot.clone();
        let running = self
            .running
            .as_ref()
            .expect("debug data responses require a running session");
        body["marrowDebug"]["debugDataBoundary"] =
            debug_data_boundary_json(&running.execution_boundary, store_snapshot);
    }

    fn child_variables(
        &mut self,
        children: Vec<crate::variables::Child>,
        store_snapshot: Option<&tooling::DataSnapshotStamp>,
    ) -> Vec<Json> {
        children
            .into_iter()
            .map(|child| {
                let counts = child
                    .expand
                    .as_ref()
                    .and_then(crate::variables::child_counts);
                let reference = child
                    .expand
                    .map(|value| {
                        self.alloc_ref(Expandable::Value {
                            value,
                            store_snapshot: store_snapshot.cloned(),
                        })
                    })
                    .unwrap_or(0);
                variable_json(
                    &child.name,
                    &child.value,
                    reference,
                    counts,
                    child.children_truncated,
                )
            })
            .collect()
    }

    fn require_stopped_run(&mut self, request: &Json) -> bool {
        match self.running.as_ref() {
            None => {
                self.respond_error(request, "not running", ERROR_NOT_RUNNING);
                false
            }
            Some(running) if running.stopped_at.is_none() => {
                self.respond_error(request, "not stopped", ERROR_NOT_STOPPED);
                false
            }
            Some(_) => true,
        }
    }

    /// `continue`: resume until the run finishes or another supported stop occurs.
    fn on_continue(&mut self, request: &Json, arguments: &Json) {
        if let Err(error) = parse_thread_id(arguments) {
            self.respond_error(request, error.message, error.contract);
            return;
        }
        if !self.require_stopped_run(request) {
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
    /// A pause request cannot interrupt a statement mid-flight. It resumes the
    /// parked run with [`Resume::Pause`], which stops before the next statement and
    /// reports `pause`.
    fn on_pause(&mut self, request: &Json, arguments: &Json) {
        if let Err(error) = parse_thread_id(arguments) {
            self.respond_error(request, error.message, error.contract);
            return;
        }
        if !self.require_stopped_run(request) {
            return;
        }
        self.respond(request, true, json!({}));
        self.event(
            "continued",
            json!({ "threadId": THREAD_ID, "allThreadsContinued": true }),
        );
        self.resume_run(Resume::Pause);
    }

    /// `evaluate`: watch, REPL, and hover requests evaluate checked Marrow
    /// debug expressions at the current stopped frame.
    fn on_evaluate(&mut self, request: &Json, arguments: &Json) {
        let input = match parse_evaluate_input(arguments) {
            Ok(input) => input,
            Err(error) => {
                self.respond_error(request, error.message, error.contract);
                return;
            }
        };

        let Some((file, span)) = self.current_stop_file_span() else {
            self.respond_error(request, "not stopped", ERROR_NOT_STOPPED);
            return;
        };
        let expression = match self.checked_evaluate_expression(&file, span, input.expression) {
            Ok(expression) => expression,
            Err(error) => {
                self.respond_error(request, error.message, error.contract);
                return;
            }
        };
        match self.query(Query::Evaluate {
            expression: Box::new(expression),
        }) {
            Some(QueryResult::Evaluate(Ok(snapshot))) => {
                let body = self.evaluate_body(snapshot.value, snapshot.store_snapshot);
                self.respond(request, true, body);
            }
            Some(QueryResult::Evaluate(Err(error))) => {
                self.respond_error(
                    request,
                    format!("{}: {}", error.code, error.message),
                    ERROR_EVALUATE_RUNTIME,
                );
            }
            _ => self.respond_error(request, "not stopped", ERROR_NOT_STOPPED),
        }
    }

    fn current_stop_file_span(&self) -> Option<(PathBuf, SourceSpan)> {
        let stop = self.running.as_ref()?.stopped_at.as_ref()?;
        Some((stop.file.clone()?, stop.span))
    }

    fn checked_evaluate_expression(
        &self,
        file: &Path,
        span: SourceSpan,
        source: &str,
    ) -> Result<CheckedDebugExpression, DapInputError> {
        let Some(analysis) = &self.analysis else {
            return Err(DapInputError::new("not stopped", ERROR_NOT_STOPPED));
        };
        analysis
            .checked_debug_expression(file, span, source)
            .map_err(|diagnostics| {
                DapInputError::new(
                    diagnostic_message(EVALUATE_EXPRESSION_INVALID, &diagnostics),
                    ERROR_EVALUATE_EXPRESSION_INVALID,
                )
            })
    }

    fn evaluate_body(
        &mut self,
        value: DebugValue,
        store_snapshot: Option<tooling::DataSnapshotStamp>,
    ) -> Json {
        let result = value.preview();
        let counts = crate::variables::child_counts(&value);
        let children_truncated =
            crate::variables::is_expandable(&value).then(|| value.children_truncated());
        let reference = if crate::variables::is_expandable(&value) {
            self.alloc_ref(Expandable::Value {
                value: value.clone(),
                store_snapshot: store_snapshot.clone(),
            })
        } else {
            0
        };
        let mut body = json!({
            "result": result,
            "variablesReference": reference,
            "presentationHint": debug_admin_presentation_hint(),
        });
        if let Some(counts) = counts {
            if let Some(named) = counts.named {
                body["namedVariables"] = json!(named);
            }
            if let Some(indexed) = counts.indexed {
                body["indexedVariables"] = json!(indexed);
            }
        }
        if let Some(children_truncated) = children_truncated {
            body["marrowDebug"] = json!({
                "childrenTruncated": children_truncated,
            });
        }
        if let Some(store_snapshot) = store_snapshot {
            let store_snapshot =
                marrow_lsp_core::data_explorer::data_generation_stamp_to_json(&store_snapshot);
            self.attach_debug_data_boundary(&mut body, store_snapshot);
        }
        body
    }

    /// `disconnect`: unwind the run (rolling back any open transaction) and end
    /// the session.
    fn on_disconnect(&mut self, request: &Json) {
        self.respond(request, true, json!({}));
        self.terminate_session();
    }

    /// `terminate`: end any live run, including one that has already resumed.
    fn on_terminate(&mut self, request: &Json) {
        if self.running.is_none() {
            self.respond_error(request, "not running", ERROR_NOT_RUNNING);
            return;
        }
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
            Ok(RunEvent::Output(_)) => None,
            _ => None,
        }
    }

    /// Mark the parked run as logically resumed and clear stop-scoped references.
    /// The protocol loop flushes the physical resume after draining queued client
    /// requests, so a pipelined terminate can cancel the wake-up before user code
    /// executes.
    fn resume_run(&mut self, mode: Resume) {
        self.expandables.clear();
        if let Some(running) = self.running.as_mut() {
            running.stopped_at = None;
            running.pending_resume = Some(mode);
        }
    }

    /// Send a staged resume once queued client requests have had a chance to run.
    /// Returns `true` when the run was woken or collected after a send failure.
    pub fn flush_resume(&mut self) -> bool {
        let send = match self.running.as_mut() {
            Some(running) if running.terminating => {
                running.pending_resume = None;
                return false;
            }
            Some(running) => running
                .pending_resume
                .take()
                .map(|mode| running.control.send(Control::Resume(mode))),
            None => None,
        };
        match send {
            Some(Ok(())) => true,
            Some(Err(_)) => {
                self.finish_run();
                true
            }
            None => false,
        }
    }

    /// Pump one pending run-thread event without blocking. Returns `true` when
    /// an event was consumed or the finished run was collected.
    pub fn pump_run_event(&mut self) -> bool {
        let event = match self.running.as_ref() {
            Some(running) => running.events.try_recv(),
            None => return false,
        };
        match event {
            Ok(event) => {
                self.handle_run_event(event);
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.finish_run();
                true
            }
        }
    }

    /// Read run-thread events until the run parks at a stop or finishes. Output
    /// events are forwarded as they arrive.
    fn pump_until_stopped_or_done(&mut self) {
        loop {
            let event = match self.running.as_ref() {
                Some(running) => running.events.recv(),
                None => return,
            };
            match event {
                Ok(event) => match self.handle_run_event(event) {
                    RunPump::ParkedOrDone => return,
                    RunPump::Continue => continue,
                },
                // The events channel closed: the run-thread finished. Collect its
                // outcome and end the session.
                Err(_) => {
                    self.finish_run();
                    return;
                }
            }
        }
    }

    fn handle_run_event(&mut self, event: RunEvent) -> RunPump {
        match event {
            RunEvent::Stopped(info) => {
                if self
                    .running
                    .as_ref()
                    .is_some_and(|running| running.terminating)
                {
                    return RunPump::Continue;
                }
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
                RunPump::ParkedOrDone
            }
            RunEvent::Output(output) => {
                self.event(
                    "output",
                    json!({ "category": "console", "output": logpoint_output(output) }),
                );
                RunPump::Continue
            }
            // An Answer here would be a protocol-thread bug; answers are only
            // received inside `query`.
            RunEvent::Answer(_) => RunPump::Continue,
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
        if let Some(running) = self.running.as_mut() {
            running.terminate.store(true, Ordering::Relaxed);
            running.stopped_at = None;
            running.pending_resume = None;
            running.terminating = true;
            let _ = running.control.send(Control::Terminate);
        }
        // Drain any final events and join.
        self.pump_until_stopped_or_done();
        if self.running.is_some() {
            self.finish_run();
        }
        self.done = true;
    }

    fn clear_launch_configuration(&mut self) {
        self.pending = None;
        self.analysis = None;
        self.stop_points = None;
        self.breakpoints.clear();
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
    counts: Option<ChildCounts>,
    children_truncated: Option<bool>,
) -> Json {
    let mut variable = json!({
        "name": name,
        "value": value,
        "presentationHint": debug_admin_presentation_hint(),
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
    if let Some(children_truncated) = children_truncated {
        variable["marrowDebug"] = json!({
            "childrenTruncated": children_truncated,
        });
    }
    variable
}

fn variables_body(variables: Vec<Json>) -> Json {
    json!({ "variables": variables })
}

fn logpoint_output(mut output: String) -> String {
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output
}

fn debug_admin_presentation_hint() -> Json {
    json!({
        "kind": "data",
        "attributes": ["readOnly"],
    })
}

fn diagnostic_message(base: &str, diagnostics: &[marrow_check::CheckDiagnostic]) -> String {
    let details = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    if details.is_empty() {
        base.to_string()
    } else {
        format!("{base}: {details}")
    }
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

fn parse_source_reference(arguments: &Json) -> Result<i64, DapInputError> {
    let Some(reference) = arguments
        .get("sourceReference")
        .and_then(Json::as_i64)
        .filter(|reference| *reference > 0)
    else {
        return Err(DapInputError::new(
            SOURCE_REFERENCE_INVALID,
            ERROR_SOURCE_REFERENCE_INVALID,
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
        .and_then(Json::as_i64)
        .is_some_and(|reference| reference > 0)
    {
        let reference = source
            .get("sourceReference")
            .and_then(Json::as_i64)
            .expect("positive sourceReference was checked");
        return Ok(BreakpointSource::SourceReference(reference));
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
    if expression.trim().is_empty() {
        return Err(DapInputError::new(
            EVALUATE_EXPRESSION_INVALID,
            ERROR_EVALUATE_EXPRESSION_INVALID,
        ));
    }
    if let Some(context) = arguments.get("context") {
        let context = context.as_str().ok_or_else(|| {
            DapInputError::new(EVALUATE_CONTEXT_INVALID, ERROR_EVALUATE_CONTEXT_INVALID)
        })?;
        match context {
            "watch" | "repl" | "hover" => {}
            _ => {
                return Err(DapInputError::new(
                    EVALUATE_CONTEXT_INVALID,
                    ERROR_EVALUATE_CONTEXT_INVALID,
                ));
            }
        }
    }
    Ok(EvaluateInput { expression })
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
        request: breakpoint_request(point),
    }
}

fn breakpoint_request(point: &Json) -> BreakpointRequest {
    let condition = match point.get("condition") {
        Some(Json::String(condition)) if !condition.trim().is_empty() => {
            Some(condition.to_string())
        }
        Some(Json::Null) | None | Some(Json::String(_)) => None,
        Some(_) => return BreakpointRequest::InvalidCondition,
    };
    let hit_target = match point.get("hitCondition") {
        Some(Json::String(hit)) => match parse_hit_condition(hit) {
            Some(target) => Some(target),
            None => return BreakpointRequest::InvalidHitCondition,
        },
        Some(Json::Null) | None => None,
        Some(_) => return BreakpointRequest::InvalidHitCondition,
    };
    let log_message = match point.get("logMessage") {
        Some(Json::String(message)) => match parse_log_message(message) {
            Some(message) => Some(message),
            None => return BreakpointRequest::InvalidLogMessage,
        },
        Some(Json::Null) | None => None,
        Some(_) => return BreakpointRequest::InvalidLogMessage,
    };
    BreakpointRequest::Valid(BreakpointSpec {
        condition,
        hit_target,
        log_message,
    })
}

fn parse_hit_condition(value: &str) -> Option<u64> {
    if value.is_empty() || !value.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let target = value.parse::<u64>().ok()?;
    (target > 0).then_some(target)
}

fn parse_log_message(value: &str) -> Option<ParsedLogMessage> {
    if value.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    let mut rest = value;
    loop {
        let Some(open) = rest.find('{') else {
            if rest.contains('}') {
                return None;
            }
            if !rest.is_empty() {
                parts.push(LogMessagePart::Literal(rest.to_string()));
            }
            return Some(ParsedLogMessage::new(parts));
        };
        if open > 0 {
            let literal = &rest[..open];
            if literal.contains('}') {
                return None;
            }
            parts.push(LogMessagePart::Literal(literal.to_string()));
        }
        let after_open = &rest[open + 1..];
        let close = after_open.find('}')?;
        let expression = after_open[..close].trim();
        if expression.is_empty() {
            return None;
        }
        parts.push(LogMessagePart::Expression(expression.to_string()));
        rest = &after_open[close + 1..];
        if rest.starts_with('}') {
            return None;
        }
    }
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
    use std::time::Duration;

    use super::*;

    fn write_durable_debug_project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        let source = "module shelf\n\
                      \n\
                      resource Book\n\
                      \x20   required title: string\n\
                      store ^books(id: int): Book\n\
                      fn storedTitle(id: int): string\n\
                      \x20   return ^books(id).title ?? \"\"\n\
                      pub fn main()\n\
                      \x20   const id: int = 1\n\
                      \x20   const title: string = \"Dune\"\n\
                      \x20   print(title)\n";
        std::fs::write(src.join("shelf.mw"), source).unwrap();
        std::fs::write(
            dir.path().join("marrow.json"),
            "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"native\", \"dataDir\": \"data\" } }",
        )
        .unwrap();
        dir
    }

    fn stopped_info() -> StopInfo {
        StopInfo {
            reason: crate::step::StopReason::Entry,
            span: SourceSpan::default(),
            file: None,
            line: 1,
            column: 1,
            depth: 0,
        }
    }

    fn session_with_running(
        stopped_at: Option<StopInfo>,
        pending_resume: Option<Resume>,
    ) -> (Session<Vec<u8>>, std::sync::mpsc::Receiver<&'static str>) {
        let (control_tx, control_rx) = std::sync::mpsc::channel();
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            while let Ok(control) = control_rx.recv() {
                match control {
                    Control::Resume(_) => {
                        let _ = seen_tx.send("resume");
                    }
                    Control::Terminate => {
                        let _ = seen_tx.send("terminate");
                        break;
                    }
                    Control::Query(_) | Control::SetBreakpoints(_) => {
                        let _ = seen_tx.send("other");
                    }
                }
            }
            drop(events_tx);
            None
        });
        let mut session = Session::new(Vec::new());
        session.running = Some(Running {
            handle,
            control: control_tx,
            events: events_rx,
            terminate: Arc::new(AtomicBool::new(false)),
            execution_boundary: json!({ "sourceAnalysisGeneration": null }),
            stopped_at,
            pending_resume,
            terminating: false,
        });
        (session, seen_rx)
    }

    fn first_dap_message(output: &str) -> Json {
        let mut reader = std::io::Cursor::new(output.as_bytes());
        crate::protocol::read_message(&mut reader).unwrap()
    }

    #[test]
    fn log_message_parser_splits_literals_and_expressions() {
        let template = parse_log_message("title { title } id {id}").expect("template");
        assert_eq!(template.parts().len(), 4);
        assert!(matches!(
            &template.parts()[0],
            LogMessagePart::Literal(value) if value == "title "
        ));
        assert!(matches!(
            &template.parts()[1],
            LogMessagePart::Expression(value) if value == "title"
        ));
        assert!(matches!(
            &template.parts()[2],
            LogMessagePart::Literal(value) if value == " id "
        ));
        assert!(matches!(
            &template.parts()[3],
            LogMessagePart::Expression(value) if value == "id"
        ));
    }

    #[test]
    fn log_message_parser_rejects_empty_and_unmatched_braces() {
        for message in [
            "",
            "title {}",
            "title { }",
            "title {name",
            "title name}",
            "bad } {title}",
        ] {
            assert!(
                parse_log_message(message).is_none(),
                "message should be rejected: {message:?}"
            );
        }
    }

    #[test]
    fn checked_evaluate_expression_admits_durable_data_access() {
        let dir = write_durable_debug_project();
        let launch = crate::project::prepare(dir.path(), None, &[]).expect("launch");
        let stop_points = StopPointIndex::from_runtime(
            launch.session.runtime_program(),
            &launch.analysis_snapshot,
        );
        let file = dir.path().join("src").join("shelf.mw");
        let stop = stop_points
            .stops(&normalize_existing_path(file), 11)
            .and_then(|stops| stops.first())
            .expect("line 11 stop");
        let mut session = Session::new(Vec::new());
        session.analysis = Some(launch.analysis_snapshot);

        let direct = session
            .checked_evaluate_expression(&stop.file, stop.span, "(^books(id).title ?? \"\")")
            .unwrap_or_else(|_| panic!("durable data expression checks through Marrow"));
        assert!(matches!(
            direct.data_access(),
            marrow_check::DebugExpressionDataAccess::RequiresDurableData
        ));

        let helper = session
            .checked_evaluate_expression(&stop.file, stop.span, "storedTitle(id) == \"Dune\"")
            .unwrap_or_else(|_| {
                panic!("helper-mediated durable data expression checks through Marrow")
            });
        assert!(matches!(
            helper.ty(),
            marrow_check::MarrowType::Primitive(marrow_check::ScalarType::Bool)
        ));
    }

    #[test]
    fn conditional_breakpoint_without_analysis_reports_invalid_state() {
        let dir = write_durable_debug_project();
        let launch = crate::project::prepare(dir.path(), None, &[]).expect("launch");
        let file = dir.path().join("src").join("shelf.mw");
        let mut session = Session::new(Vec::new());
        session.stop_points = Some(StopPointIndex::from_runtime(
            launch.session.runtime_program(),
            &launch.analysis_snapshot,
        ));
        let request = json!({ "seq": 1, "command": "setBreakpoints" });

        session.on_set_breakpoints(
            &request,
            &json!({
                "source": { "path": file.display().to_string() },
                "breakpoints": [{ "line": 11, "condition": "title == \"Dune\"" }]
            }),
        );

        let output = String::from_utf8(session.out).unwrap();
        let (_, body) = output.split_once("\r\n\r\n").unwrap();
        let response: Json = serde_json::from_str(body).unwrap();
        let breakpoint = &response["body"]["breakpoints"][0];
        assert_eq!(breakpoint["verified"], false, "{response}");
        assert_eq!(breakpoint["line"], 11, "{response}");
        assert_eq!(
            breakpoint["marrowContract"]["code"], "dap.breakpoint.unverified",
            "{response}"
        );
        assert_eq!(
            breakpoint["marrowContract"]["status"], "invalid-state",
            "{response}"
        );
        assert!(
            breakpoint["marrowContract"].get("blockedOn").is_none(),
            "{response}"
        );
    }

    #[test]
    fn queued_terminate_cancels_staged_resume_before_wake() {
        let (mut session, seen) = session_with_running(Some(stopped_info()), None);
        let cont = json!({
            "seq": 1,
            "type": "request",
            "command": "continue",
            "arguments": { "threadId": 1 },
        });
        session.handle(&cont);
        assert!(seen.try_recv().is_err());

        let terminate = json!({
            "seq": 2,
            "type": "request",
            "command": "terminate",
            "arguments": {},
        });
        session.handle(&terminate);

        assert_eq!(
            seen.recv_timeout(Duration::from_secs(1)).unwrap(),
            "terminate"
        );
        assert!(seen.try_recv().is_err());
        assert!(session.done());
    }

    #[test]
    fn terminate_after_physical_resume_is_acknowledged() {
        let (mut session, seen) = session_with_running(None, None);
        let terminate = json!({
            "seq": 1,
            "type": "request",
            "command": "terminate",
            "arguments": {},
        });
        session.handle(&terminate);

        assert_eq!(
            seen.recv_timeout(Duration::from_secs(1)).unwrap(),
            "terminate"
        );
        assert!(session.done());
        let output = String::from_utf8(session.out).unwrap();
        let response = first_dap_message(&output);
        assert_eq!(response["success"], true, "{response}");
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
        assert_eq!(
            breakpoint["marrowContract"]["code"], "dap.breakpoint.unverified",
            "{response}"
        );
        assert_eq!(
            breakpoint["marrowContract"]["status"], "invalid-state",
            "{response}"
        );
        assert!(
            breakpoint["marrowContract"].get("blockedOn").is_none(),
            "{response}"
        );
    }
}
