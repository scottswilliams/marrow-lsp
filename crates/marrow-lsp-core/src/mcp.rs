//! The agent-facing tool surface: one transport-free function per MCP tool.
//!
//! The MCP server ([`marrow-mcp`](../../marrow_mcp/index.html)) is a thin stdio
//! transport; every tool it exposes calls one function here, which orchestrates
//! the canonical marrow crates (and this core's [`workspace`](crate::workspace),
//! [`store`](crate::store), [`completion`](crate::completion) helpers) and returns
//! a `serde_json::Value`. No analysis lives in the binary — it lands here first,
//! exactly as a new LSP capability does, so the two transports share one brain.
//!
//! Two boundaries are enforced here, not in the transport:
//!
//! - **Data access** ([`saved_roots`], [`surface_read`], [`surface_write`]) reads
//!   a project's real stored tree, so the transport gates it behind an explicit
//!   opt-in and passes `allow_data = false` otherwise; the function then returns
//!   a clear refusal envelope and never touches the store. [`surface_routes`]
//!   reads only source/catalog facts and stays outside this gate.
//! - **Execution** ([`run`]) evaluates a function through Marrow's fresh-memory
//!   project session — never the project's real store — under a locked-down
//!   [`Host`] that grants only a deterministic clock and a captured log: no
//!   filesystem, no environment, no maintenance.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use marrow_check::{
    tooling::{
        SourceEnumNamespaceCompletionFact, SourceModuleNamespaceCompletionFact,
        SourceNamespaceCompletionFact, SourceNamespaceEnumMemberStatus,
        SourceNamespaceFunctionCompletion, source_namespace_completion_file_fact,
    },
    type_at,
};
use marrow_json::resource_schema::{
    RESOURCE_SCHEMA_PROFILE_VERSION, resource_schema_for_name as marrow_resource_schema_for_name,
};
use marrow_json::surface::{
    SURFACE_OPERATION_PROFILE_VERSION, SURFACE_ROUTE_PROFILE_VERSION, SurfaceAbiJson,
    SurfaceOperationKind, SurfaceOperationRequestJson, SurfaceRouteManifestJson,
    execute_project_surface_operation, execute_project_surface_operation_read_only,
};
use marrow_run::{
    CheckedEntryCall, EntryArgument, EntryDescriptor, EntryInvocation, Host, ProjectInvokeError,
    ProjectOpen, ProjectSession, ProjectSessionError, ProjectSurfaceReadSession,
    ProjectSurfaceSession, SessionEntry, StoreStamp,
};
use serde_json::{Value as Json, json};

use crate::completion::completion;
use crate::data_explorer::{
    DataChildrenRequest, DataReadRequest, session_data_child_views_page, session_data_read,
};
use crate::diagnostics::path_to_url;
use crate::documents::Documents;
use crate::positions::Position;
use crate::store::{SavedDataSession, open_saved_data_session};
use crate::types::render_type;
use crate::workspace::Workspace;

pub const COMPLETION_MISSING_FACTS: &[&str] = &["canonical completion-context facts"];
pub const SAVED_DATA_MISSING_FACTS: &[&str] =
    &["integrity and repair facts", "serve/attach data boundaries"];
pub const DATA_CHILDREN_MISSING_FACTS: &[&str] =
    &["integrity and repair facts", "serve/attach data boundaries"];
pub const DATA_READ_MISSING_FACTS: &[&str] =
    &["integrity and repair facts", "serve/attach data boundaries"];
pub const DATA_INTEGRITY_MISSING_FACTS: &[&str] = &[
    "source/store compatibility verdicts",
    "drift witnesses",
    "typed repair facts",
    "production validation contracts",
];
const SOURCE_NAMESPACE_COMPLETION_PROFILE_VERSION: &str = "source.namespace.completion.v1";

fn production_contract(description: &str) -> Json {
    json!({
        "status": "ready",
        "stableProductionApi": true,
        "description": description,
        "missingFacts": [],
    })
}

fn presentation_contract(description: &str, missing_facts: &[&str]) -> Json {
    json!({
        "status": "presentation-only",
        "stableProductionApi": false,
        "description": description,
        "missingFacts": missing_facts,
    })
}

fn completion_contract() -> Json {
    presentation_contract("development helper", COMPLETION_MISSING_FACTS)
}

fn namespace_completion_contract() -> Json {
    let mut contract = production_contract("source namespace completion fact");
    contract["basis"] = json!("Marrow source namespace completion facts");
    contract
}

fn resource_schema_contract() -> Json {
    let mut contract = production_contract("resource schema DTO");
    contract["basis"] = json!("Marrow resource schema DTO");
    contract
}

fn surface_routes_contract(scope: SurfaceRouteScope) -> Json {
    let description = match scope {
        SurfaceRouteScope::ReadOnly => "surface read route manifest",
        SurfaceRouteScope::All => "surface route manifest",
    };
    let mut contract = production_contract(description);
    contract["basis"] = json!("Marrow surface route manifest DTO");
    contract["dataAccess"] = json!("not-required");
    contract["operations"] = match scope {
        SurfaceRouteScope::ReadOnly => json!("read-only"),
        SurfaceRouteScope::All => json!("read/update/action"),
    };
    contract
}

fn surface_read_contract() -> Json {
    let mut contract = production_contract("surface read operation");
    contract["basis"] = json!("Marrow surface operation read-only executor");
    contract["dataAccess"] = json!("gated");
    contract["operations"] = json!("read-only");
    contract
}

fn surface_write_contract() -> Json {
    let mut contract = production_contract("surface write operation");
    contract["basis"] = json!("Marrow surface operation writable executor");
    contract["dataAccess"] = json!("gated");
    contract["operations"] = json!("read/update/action");
    contract
}

fn saved_data_contract() -> Json {
    presentation_contract("saved-root listing helper", SAVED_DATA_MISSING_FACTS)
}

fn data_children_contract() -> Json {
    presentation_contract("bounded typed data helper", DATA_CHILDREN_MISSING_FACTS)
}

fn data_read_contract() -> Json {
    presentation_contract("data value preview helper", DATA_READ_MISSING_FACTS)
}

fn data_integrity_contract() -> Json {
    let mut contract = presentation_contract("debug/admin advisory", DATA_INTEGRITY_MISSING_FACTS);
    contract["basis"] = json!("Marrow integrity sample DTO");
    contract
}

fn run_contract() -> Json {
    let mut contract = production_contract("sandboxed execution API");
    contract["basis"] = json!("Marrow execution boundary and run DTOs");
    contract["sandbox"] = json!("fresh-in-memory-store");
    contract["host"] = json!("locked-down");
    contract["realStore"] = json!("never-touched");
    contract["testBoundary"] = json!("Marrow test session execution boundary");
    contract
}

fn with_contract(mut result: Json, contract: Json) -> Json {
    if let Some(object) = result.as_object_mut() {
        object.insert("contract".to_string(), contract);
    }
    result
}

/// Load the project `file` belongs to (walking up to its `marrow.json`), check it,
/// and return the workspace holding the snapshot plus the file's absolute path.
/// `source`, when given, overlays the file's on-disk text so analysis reflects an
/// agent's unsaved edit. A file outside any project, or one that cannot be checked,
/// yields the error string for the tool's diagnostics envelope.
fn load_project(file: &Path, source: Option<&str>) -> Result<(Workspace, PathBuf), String> {
    let file = file.to_path_buf();
    let mut documents = Documents::new();
    if let Some(source) = source {
        // Overlay the buffer the same way the LSP overlays an open editor buffer,
        // so a project file with an unsaved edit checks against that edit.
        let url = path_to_url(&file).ok_or_else(|| "the file path is not absolute".to_string())?;
        documents.open(url, source.to_string());
    }
    let mut workspace = Workspace::new();
    workspace
        .recompute(&file, &documents)
        .map_err(|error| error.to_string())?;
    Ok((workspace, file))
}

/// One diagnostic in the agent-facing JSON: a stable dotted `code`, a severity
/// word, the message, and a zero-based UTF-16 range — the same range an editor
/// sees, so a fix the agent computes lands on the right characters.
fn diagnostic_json(diagnostic: &lsp_types::Diagnostic) -> Json {
    let code = match &diagnostic.code {
        Some(lsp_types::NumberOrString::String(code)) => code.clone(),
        Some(lsp_types::NumberOrString::Number(code)) => code.to_string(),
        None => String::new(),
    };
    let severity = match diagnostic.severity {
        Some(lsp_types::DiagnosticSeverity::ERROR) => "error",
        Some(lsp_types::DiagnosticSeverity::WARNING) => "warning",
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => "information",
        Some(lsp_types::DiagnosticSeverity::HINT) => "hint",
        _ => "error",
    };
    json!({
        "code": code,
        "severity": severity,
        "message": diagnostic.message,
        "line": diagnostic.range.start.line,
        "character": diagnostic.range.start.character,
        "endLine": diagnostic.range.end.line,
        "endCharacter": diagnostic.range.end.character,
    })
}

/// `mw_check`: the agent's compile-error feedback. A `file` (a path inside a
/// project) is checked through a [`Workspace`] — `analyze_project` over the whole
/// project, so cross-module and saved-schema errors surface, optionally overlaying
/// `source` for an unsaved edit. A bare `source` with no `file` is parsed standalone
/// via `marrow_syntax::parse_source`, surfacing only syntax errors (there is no
/// project to check against). Returns `{ diagnostics: [...] }`.
pub fn check(file: Option<&Path>, source: Option<&str>) -> Json {
    match file {
        Some(file) => match load_project(file, source) {
            Ok((workspace, file)) => {
                let snapshot = workspace.latest().expect("recompute stored a snapshot");
                // Only the requested file's diagnostics: an agent asked about one
                // file, and the rest of the project's problems are noise to it.
                let diagnostics: Vec<Json> = snapshot
                    .report
                    .diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.file == file)
                    .map(|diagnostic| {
                        let index = crate::positions::LineIndex::new(
                            source
                                .map(str::to_string)
                                .unwrap_or_else(|| read_to_string(&diagnostic.file)),
                        );
                        diagnostic_json(&crate::diagnostics::diagnostic_to_lsp(
                            diagnostic.code,
                            diagnostic.severity,
                            diagnostic.span,
                            &diagnostic.message,
                            None,
                            &index,
                        ))
                    })
                    .collect();
                json!({ "diagnostics": diagnostics })
            }
            Err(error) => json!({ "diagnostics": [], "error": error }),
        },
        None => check_snippet(source.unwrap_or_default()),
    }
}

/// Parse a bare snippet standalone and report its syntax errors. No project, so no
/// type or saved-schema checking — the agent gets the parser's own diagnostics,
/// which is the most that is sound for a fragment with no module context.
fn check_snippet(source: &str) -> Json {
    let parsed = marrow_syntax::parse_source(source);
    let index = crate::positions::LineIndex::new(source);
    let diagnostics: Vec<Json> = parsed
        .diagnostics
        .iter()
        .map(|diagnostic| {
            diagnostic_json(&crate::diagnostics::diagnostic_to_lsp(
                diagnostic.code,
                diagnostic.severity,
                diagnostic.span,
                &diagnostic.message,
                diagnostic.help.as_deref(),
                &index,
            ))
        })
        .collect();
    json!({ "diagnostics": diagnostics })
}

/// `mw_type_at`: the type of the expression at a zero-based UTF-16 line/character
/// in `file`, via `marrow_check::type_at`. `{ "type": "<spelling>" }`, or
/// `{ "type": null }` when no expression covers the position (or the file does not
/// check). The agent uses this to confirm what a sub-expression evaluates to.
pub fn type_at_position(file: &Path, line: u32, character: u32) -> Json {
    let (workspace, file) = match load_project(file, None) {
        Ok(loaded) => loaded,
        Err(error) => return json!({ "type": Json::Null, "error": error }),
    };
    let snapshot = workspace.latest().expect("recompute stored a snapshot");
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == file) else {
        return json!({ "type": Json::Null });
    };
    // The parse was built from the file's on-disk text; index that same text so the
    // position maps to the offset the parse covers.
    let source = read_to_string(&file);
    let index = crate::positions::LineIndex::new(source);
    let offset = index.offset(Position { line, character });
    let ty = type_at(&snapshot.program, &file, &analyzed.parsed, offset);
    json!({ "type": ty.map(|ty| render_type(&ty)) })
}

/// `mw_complete`: the completion items at a position in `file`, via the core
/// [`completion`] classifier — the same context-aware list the editor offers
/// (in-scope names, resource fields, saved roots, `std::` ops, keywords). Each item
/// is `{ label, kind, detail? }`. The file is checked so the snapshot's program
/// backs schema- and scope-aware modes.
pub fn complete(file: &Path, line: u32, character: u32) -> Json {
    let contract = completion_contract();
    let (workspace, file) = match load_project(file, None) {
        Ok(loaded) => loaded,
        Err(error) => return with_contract(json!({ "items": [], "error": error }), contract),
    };
    let snapshot = workspace.latest().expect("recompute stored a snapshot");
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == file) else {
        return with_contract(json!({ "items": [] }), contract);
    };
    // The classifier reads the cached lex's spans against this text, and the parse
    // came from the same on-disk text, so read and index that text.
    let source = read_to_string(&file);
    let index = crate::positions::LineIndex::new(&source);
    let offset = index.offset(Position { line, character });
    let lexed = marrow_syntax::lex_source(&source);
    let items: Vec<Json> = completion(
        &snapshot.program,
        &file,
        &source,
        &analyzed.parsed,
        &lexed,
        offset,
    )
    .into_iter()
    .map(|item| {
        json!({
            "label": item.label,
            "kind": item.kind.map(completion_kind_name),
            "detail": item.detail,
        })
    })
    .collect();
    with_contract(json!({ "items": items }), contract)
}

/// `mw_namespace_complete`: Marrow's source namespace completion fact for a
/// concrete module or enum qualifier. This is a production fact surface, not the
/// broad editor completion helper: no cursor recovery, source overlay, stdlib
/// fallback, or `CompletionItem` presentation is accepted here.
pub fn namespace_complete(file: &Path, qualifier: &[String]) -> Json {
    let contract = namespace_completion_contract();
    let (workspace, file) = match load_project(file, None) {
        Ok(loaded) => loaded,
        Err(error) => {
            return with_contract(empty_namespace_completion_result(Some(error)), contract);
        }
    };
    let snapshot = workspace.latest().expect("recompute stored a snapshot");
    let Some(analyzed) = snapshot.files.iter().find(|f| f.path == file) else {
        return with_contract(empty_namespace_completion_result(None), contract);
    };
    let result = match source_namespace_completion_file_fact(
        &snapshot.program,
        &file,
        &analyzed.parsed.file,
        qualifier,
    ) {
        Some(SourceNamespaceCompletionFact::Module(fact)) => module_namespace_fact_json(&fact),
        Some(SourceNamespaceCompletionFact::Enum(fact)) => enum_namespace_fact_json(&fact),
        Some(
            SourceNamespaceCompletionFact::StandardLibraryRoot(_)
            | SourceNamespaceCompletionFact::StandardLibraryModule(_),
        ) => empty_namespace_completion_result(None),
        None => empty_namespace_completion_result(None),
    };
    with_contract(result, contract)
}

fn empty_namespace_completion_result(error: Option<String>) -> Json {
    let mut result = json!({
        "profile_version": SOURCE_NAMESPACE_COMPLETION_PROFILE_VERSION,
        "kind": Json::Null,
        "module": Json::Null,
        "resources": [],
        "enums": [],
        "functions": [],
        "enum_name": Json::Null,
        "members": [],
    });
    if let Some(error) = error {
        result["error"] = json!(error);
    }
    result
}

fn module_namespace_fact_json(fact: &SourceModuleNamespaceCompletionFact) -> Json {
    json!({
        "profile_version": SOURCE_NAMESPACE_COMPLETION_PROFILE_VERSION,
        "kind": "module",
        "module": &fact.module,
        "resources": fact.resources.iter().map(|resource| {
            json!({ "name": &resource.name, "docs": &resource.docs })
        }).collect::<Vec<_>>(),
        "enums": fact.enums.iter().map(|enum_schema| {
            json!({ "name": &enum_schema.name, "docs": &enum_schema.docs })
        }).collect::<Vec<_>>(),
        "functions": fact.functions.iter().map(namespace_function_json).collect::<Vec<_>>(),
        "enum_name": Json::Null,
        "members": [],
    })
}

fn namespace_function_json(function: &SourceNamespaceFunctionCompletion) -> Json {
    json!({
        "name": &function.name,
        "params": function.params.iter().map(|param| {
            json!({
                "name": &param.name,
                "type": render_type(&param.ty),
                "docs": &param.docs,
            })
        }).collect::<Vec<_>>(),
        "return_type": function.return_type.as_ref().map(render_type),
        "docs": &function.docs,
    })
}

fn enum_namespace_fact_json(fact: &SourceEnumNamespaceCompletionFact) -> Json {
    json!({
        "profile_version": SOURCE_NAMESPACE_COMPLETION_PROFILE_VERSION,
        "kind": "enum",
        "module": Json::Null,
        "resources": [],
        "enums": [],
        "functions": [],
        "enum_name": &fact.enum_name,
        "members": fact.members.iter().map(|member| {
            json!({
                "name": &member.name,
                "docs": &member.docs,
                "status": enum_member_status_json(member.status),
            })
        }).collect::<Vec<_>>(),
    })
}

fn enum_member_status_json(status: SourceNamespaceEnumMemberStatus) -> &'static str {
    match status {
        SourceNamespaceEnumMemberStatus::Selectable => "selectable",
        SourceNamespaceEnumMemberStatus::Category => "category",
        SourceNamespaceEnumMemberStatus::Group => "group",
    }
}

/// `mw_resource_schema`: Marrow's canonical JSON DTO for one resource schema.
pub fn resource_schema(file: &Path, name: &str) -> Json {
    let contract = resource_schema_contract();
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => {
            return with_contract(
                json!({
                    "profile_version": RESOURCE_SCHEMA_PROFILE_VERSION,
                    "resources": [],
                    "diagnostics": [],
                    "error": error,
                }),
                contract,
            );
        }
    };
    let Some(program) = workspace.program() else {
        return with_contract(
            json!({
                "profile_version": RESOURCE_SCHEMA_PROFILE_VERSION,
                "resources": [],
                "diagnostics": [{
                    "code": "resource.schema.missing",
                    "message": "no checked program is available for resource schema lookup"
                }],
            }),
            contract,
        );
    };
    let result = serde_json::to_value(marrow_resource_schema_for_name(program, name))
        .expect("Marrow resource schema DTO serializes");
    with_contract(result, contract)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceRouteScope {
    ReadOnly,
    All,
}

/// `mw_surface_routes`: the Marrow-owned route manifest for the checked
/// project's stable surface operations. This reads source/catalog facts only; it
/// does not open the project's data store and does not need the data-access gate.
pub fn surface_routes(file: &Path, scope: SurfaceRouteScope) -> Json {
    let contract = surface_routes_contract(scope);
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => return with_contract(empty_surface_route_manifest(error), contract),
    };
    let Some(program) = workspace.program() else {
        return with_contract(empty_surface_route_manifest("no checked program"), contract);
    };
    let abi = SurfaceAbiJson::from_program(program);
    let mut manifest = SurfaceRouteManifestJson::from_abi(&abi);
    if scope == SurfaceRouteScope::ReadOnly {
        manifest
            .routes
            .retain(|route| SurfaceOperationKind::from(&route.request).is_read());
    }
    let result = serde_json::to_value(manifest).expect("surface route manifest DTO serializes");
    with_contract(result, contract)
}

fn empty_surface_route_manifest(error: impl Into<String>) -> Json {
    json!({
        "profile_version": SURFACE_ROUTE_PROFILE_VERSION,
        "operation_profile_version": SURFACE_OPERATION_PROFILE_VERSION,
        "routes": [],
        "error": error.into(),
    })
}

/// `mw_surface_read`: execute one canonical `surface.operation.v1` read request
/// through Marrow's read-only project surface executor. The MCP data-access gate
/// is checked before project or store opening; operation bodies are admitted only
/// by the Marrow JSON DTO and executor.
pub fn surface_read(file: &Path, operation: Json, allow_data: bool) -> Result<Json, String> {
    let contract = surface_read_contract();
    if !allow_data {
        return Ok(data_disabled(contract));
    }
    let request: SurfaceOperationRequestJson = serde_json::from_value(operation)
        .map_err(|error| format!("invalid mw_surface_read operation: {error}"))?;
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => {
            return Ok(with_contract(
                json!({ "available": false, "error": error }),
                contract,
            ));
        }
    };
    let project = match workspace.project() {
        Some(project) => project,
        None => {
            return Ok(with_contract(
                json!({ "available": false, "error": "no project resolved for the file" }),
                contract,
            ));
        }
    };
    let session = match ProjectSurfaceReadSession::open(&project.root) {
        Ok(session) => session,
        Err(error) => {
            return Ok(with_contract(
                json!({ "available": false, "error": error.message() }),
                contract,
            ));
        }
    };
    let store_stamp = match session.store_stamp() {
        Ok(stamp) => store_stamp_json(stamp),
        Err(error) => {
            return Ok(with_contract(
                json!({ "available": false, "error": error.message() }),
                contract,
            ));
        }
    };
    let result = match execute_project_surface_operation_read_only(&session, &request) {
        Ok(response) => {
            json!({ "available": true, "store_stamp": store_stamp, "response": response })
        }
        Err(error) => json!({ "available": true, "store_stamp": store_stamp, "error": error }),
    };
    Ok(with_contract(result, contract))
}

/// `mw_surface_write`: execute one canonical `surface.operation.v1` request
/// through Marrow's writable project surface executor. The MCP data-access gate
/// is checked before project or store opening; operation bodies are admitted only
/// by the Marrow JSON DTO and executor.
pub fn surface_write(file: &Path, operation: Json, allow_data: bool) -> Result<Json, String> {
    let contract = surface_write_contract();
    if !allow_data {
        return Ok(data_disabled(contract));
    }
    let request: SurfaceOperationRequestJson = serde_json::from_value(operation)
        .map_err(|error| format!("invalid mw_surface_write operation: {error}"))?;
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => {
            return Ok(with_contract(
                json!({ "available": false, "error": error }),
                contract,
            ));
        }
    };
    let project = match workspace.project() {
        Some(project) => project,
        None => {
            return Ok(with_contract(
                json!({ "available": false, "error": "no project resolved for the file" }),
                contract,
            ));
        }
    };
    let session = match ProjectSurfaceSession::open(&project.root) {
        Ok(session) => session,
        Err(error) => {
            return Ok(with_contract(
                json!({ "available": false, "error": error.message() }),
                contract,
            ));
        }
    };
    let operation_result = execute_project_surface_operation(&session, &request);
    let store_stamp = match session.store_stamp() {
        Ok(stamp) => store_stamp_json(stamp),
        Err(error) => {
            return Ok(with_contract(
                json!({ "available": false, "error": error.message() }),
                contract,
            ));
        }
    };
    let result = match operation_result {
        Ok(response) => {
            json!({ "available": true, "store_stamp": store_stamp, "response": response })
        }
        Err(error) => json!({ "available": true, "store_stamp": store_stamp, "error": error }),
    };
    Ok(with_contract(result, contract))
}

fn store_stamp_json(stamp: StoreStamp) -> Json {
    json!({
        "store_uid": stamp.store_uid,
        "catalog_epoch": stamp.catalog_epoch,
        "commit_id": stamp.commit_id,
    })
}

/// `mw_saved_roots`: the project's durable saved root views, read through
/// Marrow's linked surface-read session. Gated: with `allow_data = false` the
/// function returns a refusal envelope and never opens the store. A project with
/// no native store, or a store that cannot be read right now, answers
/// `available: false`.
pub fn saved_roots(file: &Path, allow_data: bool) -> Json {
    let contract = saved_data_contract();
    if !allow_data {
        return data_disabled(contract);
    }
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => {
            return with_contract(
                json!({ "available": false, "roots": [], "error": error }),
                contract,
            );
        }
    };
    let session = match data_session(&workspace) {
        Ok(session) => session,
        Err(error) => {
            return with_contract(
                json!({ "available": false, "roots": [], "error": error }),
                contract,
            );
        }
    };
    let Some(session) = session else {
        return with_contract(json!({ "available": false, "roots": [] }), contract);
    };
    let result = serde_json::to_value(crate::data_explorer::saved_roots(Some(&session)))
        .unwrap_or_else(|_| json!({ "available": false, "roots": [] }));
    with_contract(result, contract)
}

/// `mw_data_children`: a bounded page of typed child segments under a saved-data
/// path. Gated like [`saved_roots`]: disabled data access refuses before project
/// loading, while enabled access checks the project and opens the native store
/// read-only for one shared Marrow session read.
pub fn data_children(file: &Path, request: DataChildrenRequest, allow_data: bool) -> Json {
    let contract = data_children_contract();
    if !allow_data {
        return data_disabled(contract);
    }
    let unavailable = json!({
        "available": false,
        "children": [],
        "truncated": false,
        "cursor": Json::Null,
        "store_snapshot": Json::Null,
    });
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => {
            let mut value = unavailable.clone();
            value["error"] = json!(error);
            return with_contract(value, contract);
        }
    };
    let session = match data_session(&workspace) {
        Ok(session) => session,
        Err(error) => {
            let mut value = unavailable.clone();
            value["error"] = json!(error);
            return with_contract(value, contract);
        }
    };
    let Some(session) = session else {
        return with_contract(unavailable, contract);
    };
    match session_data_child_views_page(&session, request) {
        Ok(result) => {
            let mut result =
                serde_json::to_value(result).expect("data children DTO must serialize");
            result["available"] = json!(true);
            with_contract(result, contract)
        }
        Err(error) => with_contract(
            json!({
                "available": true,
                "children": [],
                "truncated": false,
                "cursor": Json::Null,
                "store_snapshot": Json::Null,
                "error": error,
            }),
            contract,
        ),
    }
}

/// `mw_data_read`: a bounded rendered preview for one typed saved-data path.
/// Gated like [`data_children`]: disabled data access refuses before project
/// loading, while enabled access checks the project and opens the native store
/// read-only for one shared Marrow session read.
pub fn data_read(file: &Path, request: DataReadRequest, allow_data: bool) -> Json {
    let contract = data_read_contract();
    if !allow_data {
        return data_disabled(contract);
    }
    let unavailable = json!({
        "available": false,
        "presence": "absent",
        "value": Json::Null,
        "value_truncated": false,
        "store_snapshot": Json::Null,
    });
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => {
            let mut value = unavailable.clone();
            value["error"] = json!(error);
            return with_contract(value, contract);
        }
    };
    let session = match data_session(&workspace) {
        Ok(session) => session,
        Err(error) => {
            let mut value = unavailable.clone();
            value["error"] = json!(error);
            return with_contract(value, contract);
        }
    };
    let Some(session) = session else {
        return with_contract(unavailable, contract);
    };
    match session_data_read(&session, request) {
        Ok(result) => {
            let mut result = serde_json::to_value(result).expect("data read DTO must serialize");
            result["available"] = json!(true);
            with_contract(result, contract)
        }
        Err(error) => with_contract(
            json!({
                "available": true,
                "presence": "absent",
                "value": Json::Null,
                "value_truncated": false,
                "store_snapshot": Json::Null,
                "error": error,
            }),
            contract,
        ),
    }
}

fn data_session(workspace: &Workspace) -> Result<Option<SavedDataSession>, String> {
    let project = workspace
        .project()
        .ok_or_else(|| "no project resolved for the file".to_string())?;
    open_saved_data_session(project)
}

/// `mw_data_integrity`: the schema-change-impact advisory — a capped, on-demand
/// scan of the project's real stored data that flags every record the *current*
/// schema can no longer account for (an orphan path, or a value that no longer
/// decodes as its declared type). Reuses the same classification as
/// `marrow data integrity`, through a linked Marrow read session. Gated like
/// [`saved_roots`]: with `allow_data = false` it returns a refusal envelope and
/// never opens the store. A project with no native store, or a store that cannot
/// be read right now, answers `available: false`.
pub fn data_integrity(file: &Path, allow_data: bool) -> Json {
    let contract = data_integrity_contract();
    if !allow_data {
        let mut disabled = data_disabled(contract);
        disabled["store_snapshot"] = Json::Null;
        return disabled;
    }
    let unavailable = json!({
        "available": false,
        "findings": [],
        "scanned": 0,
        "truncated": false,
        "store_snapshot": Json::Null,
    });
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => {
            let mut value = unavailable.clone();
            value["error"] = json!(error);
            return with_contract(value, contract);
        }
    };
    let session = match data_session(&workspace) {
        Ok(session) => session,
        Err(error) => {
            let mut value = unavailable.clone();
            value["error"] = json!(error);
            return with_contract(value, contract);
        }
    };
    let result = serde_json::to_value(crate::data_integrity::data_integrity(session.as_ref()))
        .unwrap_or(unavailable);
    with_contract(result, contract)
}

/// The refusal a data tool returns when data access is not enabled: a clear,
/// machine-readable envelope that carries no stored data. The transport sets the
/// opt-in (an env var / launch flag); the boundary itself lives here so a missing
/// opt-in can never leak a byte regardless of transport.
fn data_disabled(contract: Json) -> Json {
    with_contract(
        json!({
            "available": false,
            "dataAccess": "disabled",
            "message": "data access not enabled; relaunch marrow-mcp with MARROW_MCP_ALLOW_DATA=1 (or --allow-data) to read stored data",
        }),
        contract,
    )
}

/// How a `mw_run` request runs `entry`: as a normal entry, or as the project's
/// test suite.
pub enum RunMode {
    /// Evaluate `entry` over a fresh store and report Marrow's run envelope.
    Run,
    /// Discover and run the project's tests, each over its own fresh store.
    Test,
}

/// `mw_run`: execute Marrow to confirm behavior — always sandboxed.
///
/// **Run mode** checks the project (`analyze_project` via the workspace), then
/// evaluates `entry` through Marrow's fresh-memory project session under a
/// locked-down [`Host`]: a fixed clock and a captured log only — no filesystem,
/// no environment, no maintenance. Typed `args` are decoded from the wire and
/// admitted by Marrow's entry descriptor for the current checked program. The
/// result carries Marrow's typed run envelope, including the returned value when
/// present, captured `output`, and diagnostics.
///
/// **Test mode** opens Marrow's test project session, then invokes every
/// discovered test case under the same locked host. Marrow gives each test its
/// own fresh in-memory store. `entry` is ignored; `args` are only accepted in run
/// mode.
///
/// The project's real store is never opened in either mode, so an agent can run
/// code with no risk to managed data.
pub fn run(file: &Path, entry: Option<&str>, args: &[Json], mode: RunMode) -> Json {
    with_contract(run_result(file, entry, args, mode), run_contract())
}

fn run_result(file: &Path, entry: Option<&str>, args: &[Json], mode: RunMode) -> Json {
    let args = match marrow_run::entry_arguments_from_json(args) {
        Ok(args) => args,
        Err(error) => return run_args_error(&error.message()),
    };
    if matches!(mode, RunMode::Test) && !args.is_empty() {
        return run_args_error("mw_run test mode does not accept entry args");
    }

    let root = match load_project_for_run(file) {
        Ok(root) => root,
        Err(error) => return json!({ "diagnostics": [{ "message": error }], "output": "" }),
    };

    match mode {
        RunMode::Run => run_entry(&root, entry, args),
        RunMode::Test => run_tests(&root),
    }
}

fn run_args_error(message: &str) -> Json {
    json!({
        "diagnostics": [{
            "code": "mcp.run.args",
            "message": message
        }],
        "output": "",
    })
}

/// Load `file`'s project far enough to find the root Marrow will run.
fn load_project_for_run(file: &Path) -> Result<PathBuf, String> {
    let (workspace, _) = load_project(file, None)?;
    let project = workspace
        .project()
        .ok_or_else(|| "no project resolved for the file".to_string())?;
    Ok(project.root.clone())
}

/// Evaluate one `entry` through Marrow's fresh-memory session under the locked
/// host, returning Marrow's run envelope DTO plus entry run facts when entry
/// admission succeeds. The fact DTO is emitted by Marrow and includes the
/// opened execution boundary with the versioned analysis generation that
/// admitted the entry.
fn run_entry(root: &Path, entry: Option<&str>, args: Vec<EntryArgument>) -> Json {
    let mut open = ProjectOpen::run().with_fresh_memory_store();
    if let Some(entry) = entry {
        open = open.with_entry_override(entry);
    }
    let session = match ProjectSession::open(root, open) {
        Ok(session) => session,
        Err(error) => {
            return run_envelope_to_json(marrow_json::run_session_error_to_json(
                &error,
                String::new(),
            ));
        }
    };
    let Some(entry) = session.run_entry() else {
        return run_envelope_to_json(marrow_json::run_session_error_to_json(
            &ProjectSessionError::NoEntry,
            String::new(),
        ));
    };
    let descriptor = match EntryDescriptor::resolve(session.runtime_program(), entry) {
        Ok(descriptor) => descriptor,
        Err(_) => return entry_descriptor_error_json(&session, entry),
    };
    let run_facts = entry_run_facts_json(&session);
    let invocation = EntryInvocation {
        identity: descriptor.identity.clone(),
        arguments: args,
    };
    let host = locked_host();
    let mut output = String::new();
    let result = match session.invoke(SessionEntry::protocol(invocation, &host, &mut output)) {
        Ok(result) => {
            let output_for_error = output.clone();
            match marrow_json::run_output_to_json(&result, output) {
                Ok(envelope) => run_envelope_to_json(envelope),
                Err(error) => run_envelope_to_json(marrow_json::run_error_to_json(
                    session.runtime_program(),
                    &ProjectInvokeError::Runtime(error),
                    output_for_error,
                )),
            }
        }
        Err(error) => run_envelope_to_json(marrow_json::run_error_to_json(
            session.runtime_program(),
            &error,
            output,
        )),
    };
    with_run_facts(result, run_facts)
}

fn entry_run_facts_json(session: &ProjectSession) -> Option<Json> {
    marrow_json::entry_run_facts_to_json(session)
        .map(|facts| serde_json::to_value(facts).expect("Marrow run facts DTO serializes"))
}

fn execution_boundary_json(session: &ProjectSession) -> Json {
    let boundary = marrow_json::execution_boundary_to_json(session);
    serde_json::to_value(boundary).expect("Marrow boundary DTO serializes")
}

/// Discover and run the project's tests through Marrow's test project session,
/// returning `{ output, diagnostics, tests: [...], executionBoundary }`.
fn run_tests(root: &Path) -> Json {
    let session = match ProjectSession::open(root, ProjectOpen::test()) {
        Ok(session) => session,
        Err(error) => {
            return merge(
                run_envelope_to_json(marrow_json::run_session_error_to_json(
                    &error,
                    String::new(),
                )),
                json!({ "tests": [] }),
            );
        }
    };
    let execution_boundary = execution_boundary_json(&session);
    let host = locked_host();
    let mut tests = Vec::new();
    for case in session.test_cases() {
        let entry = json!({ "name": case.name, "file": case.source_file.display().to_string() });
        let mut output = String::new();
        let result = match session.invoke(SessionEntry::new(&case.name, &host, &mut output)) {
            Ok(_) => merge(entry, json!({ "outcome": "passed" })),
            Err(ProjectInvokeError::Runtime(error)) if error.code() == marrow_run::RUN_ASSERT => {
                merge(
                    entry,
                    json!({ "outcome": "failed", "code": error.code(), "message": error.message, "line": error.span.line, "character": error.span.column }),
                )
            }
            Err(ProjectInvokeError::Runtime(error)) => merge(
                entry,
                json!({ "outcome": "errored", "code": error.code(), "message": error.message, "line": error.span.line, "character": error.span.column }),
            ),
            Err(error) => merge(
                entry,
                json!({ "outcome": "errored", "code": error.code(), "message": error.message() }),
            ),
        };
        tests.push(result);
    }
    json!({ "diagnostics": [], "output": "", "tests": tests, "executionBoundary": execution_boundary })
}

/// The locked-down host every sandboxed run gets: a deterministic clock (a fixed
/// epoch instant, so `std::clock::now()` is reproducible across runs) and a
/// discard log sink (so `std::log` does not fault for want of a capability). It
/// grants no filesystem, no environment, and no maintenance — the capabilities a
/// run must not have when an agent invokes it on untrusted code.
fn locked_host() -> Host {
    Host::new()
        // A fixed instant (the Unix epoch) keeps runs reproducible; the real clock
        // is a host capability a sandboxed agent run must not depend on.
        .with_clock(0)
        // A sink the log writes into and we drop, so `std::log` works without
        // leaking anywhere; granting the capability is what keeps it from faulting.
        .with_log_sink(std::rc::Rc::new(RefCell::new(String::new())))
}

fn run_envelope_to_json(envelope: marrow_json::RunEnvelopeJson) -> Json {
    serde_json::to_value(envelope).expect("Marrow run envelope DTO serializes")
}

fn with_run_facts(mut result: Json, facts: Option<Json>) -> Json {
    if let (Some(object), Some(facts)) = (result.as_object_mut(), facts) {
        object.insert("runFacts".to_string(), facts);
    }
    result
}

fn entry_descriptor_error_json(session: &ProjectSession, entry: &str) -> Json {
    match CheckedEntryCall::from_text_args(session.runtime_program(), entry, &[]) {
        Err(error) => run_envelope_to_json(marrow_json::run_error_to_json(
            session.runtime_program(),
            &ProjectInvokeError::Runtime(error),
            String::new(),
        )),
        Ok(_) => run_envelope_to_json(marrow_json::run_session_error_to_json(
            &ProjectSessionError::NoEntry,
            String::new(),
        )),
    }
}

/// Merge the fields of `extra` into `base` (a small object union for assembling a
/// test result from its identity and its outcome).
fn merge(mut base: Json, extra: Json) -> Json {
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    base
}

/// The lowercase name of a completion item kind, so the agent reads `variable`,
/// `function`, `field`, … rather than an LSP enum number.
fn completion_kind_name(kind: lsp_types::CompletionItemKind) -> &'static str {
    use lsp_types::CompletionItemKind as K;
    match kind {
        K::VARIABLE => "variable",
        K::FUNCTION => "function",
        K::FIELD => "field",
        K::METHOD => "method",
        K::MODULE => "module",
        K::STRUCT => "struct",
        K::CLASS => "class",
        K::CONSTANT => "constant",
        K::KEYWORD => "keyword",
        _ => "other",
    }
}

fn read_to_string(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests;
