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
//! - **Data access** ([`saved_roots`]) reads a project's real stored tree, so the
//!   transport gates it behind an explicit opt-in and passes `allow_data = false`
//!   otherwise; the function then returns a clear refusal envelope and never
//!   touches the store.
//! - **Execution** ([`run`]) evaluates a function through Marrow's fresh-memory
//!   project session — never the project's real store — under a locked-down
//!   [`Host`] that grants only a deterministic clock and a captured log: no
//!   filesystem, no environment, no maintenance.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use marrow_check::{CheckedProgram, type_at};
use marrow_run::{
    CheckedEntryCall, Host, ProjectInvokeError, ProjectOpen, ProjectSession, ProjectSessionError,
    RunOutput, RuntimeError, SessionEntry, Value, run_entry_with_host,
};
use marrow_schema::{IndexSchema, KeyDef, Node, NodeKind, ResourceSchema, StoreSchema, Type};
use marrow_store::tree::TreeStore;
use serde_json::{Map, Value as Json, json};

use crate::completion::completion;
use crate::data_explorer::{DataChildrenRequest, DataReadRequest};
use crate::diagnostics::path_to_url;
use crate::documents::Documents;
use crate::positions::Position;
use crate::store::LiveStore;
use crate::types::render_type;
use crate::workspace::Workspace;

/// A clamp on the saved bytes a `run`/test captures into its result. Output is a
/// program's own `print`/`write` stream; a runaway loop must not balloon the
/// reply, so it is truncated with a marker once it crosses this.
const OUTPUT_CAP: usize = 8 * 1024;
const RUN_VALUE_NODE_CAP: usize = 256;
const RUN_VALUE_STRING_CAP: usize = 8 * 1024;

pub const COMPLETION_MISSING_FACTS: &[&str] = &["canonical completion-context facts"];
pub const RESOURCE_SCHEMA_MISSING_FACTS: &[&str] = &[
    "catalog-bound resource/store/member identity",
    "presence/default facts",
    "typed protocol DTOs",
];
pub const SAVED_DATA_MISSING_FACTS: &[&str] = &[
    "catalog-bound saved-root identity",
    "versioned source/store generation",
    "watch/refresh facts",
    "integrity and repair facts",
    "serve/attach data boundaries",
];
pub const DATA_CHILDREN_MISSING_FACTS: &[&str] = &[
    "catalog-bound saved-place identity",
    "versioned source/store generation",
    "watch/refresh facts",
    "integrity and repair facts",
    "serve/attach data boundaries",
];
pub const DATA_READ_MISSING_FACTS: &[&str] = &[
    "catalog-bound saved-place identity",
    "versioned source/store generation",
    "bounded presence facts",
    "watch/refresh facts",
    "integrity and repair facts",
    "serve/attach data boundaries",
];
pub const DATA_INTEGRITY_MISSING_FACTS: &[&str] = &[
    "source/store compatibility verdicts",
    "drift witnesses",
    "typed repair facts",
    "stable production integrity DTOs",
];
pub const RUN_MISSING_FACTS: &[&str] = &[
    "canonical function-entry facts",
    "transitive effect facts",
    "durable-scope facts",
    "transaction facts",
    "runtime generation facts",
    "typed run protocol DTOs",
];

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

fn resource_schema_contract() -> Json {
    presentation_contract("development helper", RESOURCE_SCHEMA_MISSING_FACTS)
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
    presentation_contract("debug/admin advisory", DATA_INTEGRITY_MISSING_FACTS)
}

fn run_contract() -> Json {
    presentation_contract("sandboxed execution helper", RUN_MISSING_FACTS)
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

/// `mw_resource_schema`: a JSON projection of one unambiguous named resource schema. The
/// shape renders current checked schema facts for inspection only: resource name,
/// stores, identity-key declarations, indexes, and member tree. It is not a typed
/// saved-path, durable data DTO, or paged catalog API.
pub fn resource_schema(file: &Path, name: &str) -> Json {
    let contract = resource_schema_contract();
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => return with_contract(json!({ "resources": [], "error": error }), contract),
    };
    let Some(program) = workspace.program() else {
        return with_contract(json!({ "resources": [] }), contract);
    };
    let matches: Vec<(&ResourceSchema, Vec<&StoreSchema>)> = program
        .modules
        .iter()
        .flat_map(|module| {
            module
                .resources
                .iter()
                .filter(|resource| resource.name == name)
                .map(move |resource| {
                    let stores = module
                        .stores
                        .iter()
                        .filter(|store| store.resource == resource.name)
                        .collect();
                    (resource, stores)
                })
        })
        .collect();

    if matches.len() > 1 {
        return with_contract(
            json!({
                "resources": [],
                "diagnostics": [{
                    "code": "mcp.resourceSchema.identity",
                    "message": "resource schema lookup is ambiguous until Marrow exposes catalog-bound resource identity facts"
                }],
            }),
            contract,
        );
    }

    let resources: Vec<Json> = matches
        .into_iter()
        .map(|(resource, stores)| schema_to_json(resource, stores.into_iter()))
        .collect();
    with_contract(json!({ "resources": resources }), contract)
}

/// Project one resource schema to JSON: name, stores, and member tree. The member
/// tree is recursive because groups carry nested members.
fn schema_to_json<'a>(
    resource: &ResourceSchema,
    stores: impl Iterator<Item = &'a StoreSchema>,
) -> Json {
    json!({
        "name": resource.name,
        "docs": resource.docs,
        "stores": stores.map(store_to_json).collect::<Vec<_>>(),
        "members": resource.members.iter().map(node_to_json).collect::<Vec<_>>(),
    })
}

fn store_to_json(store: &StoreSchema) -> Json {
    json!({
        "root": store.root,
        "resource": store.resource,
        "docs": store.docs,
        "identityKeys": store.identity_keys.iter().map(key_def_to_json).collect::<Vec<_>>(),
        "indexes": store.indexes.iter().map(index_to_json).collect::<Vec<_>>(),
    })
}

fn key_def_to_json(key: &KeyDef) -> Json {
    json!({ "name": key.name, "type": type_name(&key.ty) })
}

/// Project one member node. A field is `{ kind: "field", type, required }`; a keyed
/// leaf is `{ kind: "leaf", type, keyParams }`; a group is `{ kind: "group",
/// keyParams, members }` with its nested members projected recursively.
fn node_to_json(node: &Node) -> Json {
    match &node.kind {
        NodeKind::Slot { ty, required } if node.key_params.is_empty() => json!({
            "name": node.name,
            "kind": "field",
            "type": type_name(ty),
            "required": required,
            "docs": node.docs,
        }),
        NodeKind::Slot { ty, .. } => json!({
            "name": node.name,
            "kind": "leaf",
            "type": type_name(ty),
            "keyParams": node.key_params.iter().map(key_def_to_json).collect::<Vec<_>>(),
            "docs": node.docs,
        }),
        NodeKind::Group => json!({
            "name": node.name,
            "kind": "group",
            "keyParams": node.key_params.iter().map(key_def_to_json).collect::<Vec<_>>(),
            "members": node.members.iter().map(node_to_json).collect::<Vec<_>>(),
            "docs": node.docs,
        }),
    }
}

fn index_to_json(index: &IndexSchema) -> Json {
    json!({
        "name": index.name,
        "args": index.args,
        "unique": index.unique,
        "docs": index.docs,
    })
}

/// `mw_saved_roots`: the project's durable saved root names, read through a
/// short-lived [`LiveStore`]. Gated: with `allow_data = false` the function
/// returns a refusal envelope and never opens the store. A project with no native
/// store, or a store that cannot be read right now, answers `available: false`.
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
    let reader = workspace.project().and_then(LiveStore::for_project);
    let Some(program) = workspace.program() else {
        return with_contract(json!({ "available": false, "roots": [] }), contract);
    };
    let result = serde_json::to_value(crate::data_explorer::saved_roots(reader.as_ref(), program))
        .unwrap_or_else(|_| json!({ "available": false, "roots": [] }));
    with_contract(result, contract)
}

/// `mw_data_children`: a bounded page of typed child segments under a saved-data
/// path. Gated like [`saved_roots`]: disabled data access refuses before project
/// loading, while enabled access checks the project and opens the native store
/// read-only for one shared Marrow tooling read.
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
    let reader = workspace.project().and_then(LiveStore::for_project);
    let program = match data_program(&workspace) {
        Ok(program) => program,
        Err(error) => {
            let mut value = unavailable.clone();
            value["error"] = json!(error);
            return with_contract(value, contract);
        }
    };
    let Some(reader) = reader else {
        return with_contract(unavailable, contract);
    };
    match reader.data_children_page(&program, request) {
        crate::store::Availability::Unavailable => with_contract(unavailable, contract),
        crate::store::Availability::Available(Ok(result)) => {
            let mut result =
                serde_json::to_value(result).expect("data children DTO must serialize");
            result["available"] = json!(true);
            with_contract(result, contract)
        }
        crate::store::Availability::Available(Err(error)) => with_contract(
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
/// read-only for one shared Marrow tooling read.
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
    let reader = workspace.project().and_then(LiveStore::for_project);
    let program = match data_program(&workspace) {
        Ok(program) => program,
        Err(error) => {
            let mut value = unavailable.clone();
            value["error"] = json!(error);
            return with_contract(value, contract);
        }
    };
    let Some(reader) = reader else {
        return with_contract(unavailable, contract);
    };
    match reader.data_read(&program, request) {
        crate::store::Availability::Unavailable => with_contract(unavailable, contract),
        crate::store::Availability::Available(Ok(result)) => {
            let mut result = serde_json::to_value(result).expect("data read DTO must serialize");
            result["available"] = json!(true);
            with_contract(result, contract)
        }
        crate::store::Availability::Available(Err(error)) => with_contract(
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

fn data_program(workspace: &Workspace) -> Result<CheckedProgram, String> {
    let project = workspace
        .project()
        .ok_or_else(|| "no project resolved for the file".to_string())?;
    let accepted = marrow_check::read_accepted_catalog_artifact(&project.root)
        .map_err(|error| error.message())?;
    marrow_check::check_project_against(&project.root, &project.config, accepted.as_ref())
        .map_err(|error| error.message())
}

/// `mw_data_integrity`: the schema-change-impact advisory — a capped, on-demand
/// scan of the project's real stored data that flags every record the *current*
/// schema can no longer account for (an orphan path, or a value that no longer
/// decodes as its declared type). Reuses the same classification as
/// `marrow data integrity`, through a short-lived [`LiveStore`]. Gated like
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
    let reader = workspace.project().and_then(LiveStore::for_project);
    let Some(program) = workspace.program() else {
        return with_contract(unavailable, contract);
    };
    let result = serde_json::to_value(crate::data_integrity::data_integrity(
        reader.as_ref(),
        program,
    ))
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
    /// Evaluate `entry` over a fresh store and report its value and output.
    Run,
    /// Discover and run the project's tests, each over its own fresh store.
    Test,
}

/// `mw_run`: execute Marrow to confirm behavior — always sandboxed.
///
/// **Run mode** checks the project (`analyze_project` via the workspace), then
/// evaluates `entry` (`"module::fn"`) through Marrow's fresh-memory project
/// session under a locked-down [`Host`]: a fixed clock and a captured log only —
/// no filesystem, no environment, no maintenance. Non-empty `args` are blocked
/// because Marrow does not yet expose stable typed run argument facts. The result
/// carries the returned `value`, the captured `output`, and any check
/// `diagnostics`.
///
/// **Test mode** runs `check_tests` then, for every public zero-parameter test
/// function, runs it over its *own* fresh [`TreeStore`] under the same locked host,
/// reporting per-test pass/fail/error. `entry` is ignored; non-empty `args` are
/// blocked for both modes until Marrow owns typed run arguments.
///
/// The project's real store is never opened in either mode, so an agent can run
/// code with no risk to managed data.
pub fn run(file: &Path, entry: Option<&str>, args: &[Json], mode: RunMode) -> Json {
    with_contract(run_result(file, entry, args, mode), run_contract())
}

fn run_result(file: &Path, entry: Option<&str>, args: &[Json], mode: RunMode) -> Json {
    if !args.is_empty() {
        return run_args_refusal();
    }

    let (workspace, root, config) = match load_project_for_run(file) {
        Ok(loaded) => loaded,
        Err(error) => return json!({ "diagnostics": [{ "message": error }], "output": "" }),
    };
    let snapshot = workspace.latest().expect("recompute stored a snapshot");

    // Surface check errors rather than running stale or broken code: a program
    // with errors should not be executed, and the diagnostics are the answer.
    let diagnostics: Vec<Json> = snapshot
        .report
        .diagnostics
        .iter()
        .filter(|d| d.severity == marrow_syntax::Severity::Error)
        .map(|d| {
            json!({
                "code": d.code,
                "message": d.message,
                "file": d.file.display().to_string(),
                "line": d.span.line,
                "character": d.span.column,
            })
        })
        .collect();
    if !diagnostics.is_empty() {
        return json!({ "diagnostics": diagnostics, "output": "" });
    }

    match mode {
        RunMode::Run => run_entry(&root, entry),
        RunMode::Test => run_tests(&root, &config, &snapshot.program),
    }
}

fn run_args_refusal() -> Json {
    json!({
        "diagnostics": [{
            "code": "mcp.run.args",
            "message": "mw_run args are blocked until typed run argument facts from Marrow are available"
        }],
        "output": "",
    })
}

/// Load `file`'s project for a run: the checked workspace, the project root, and the
/// parsed config. Distinct from [`load_project`] because test discovery and the
/// snapshot both need the root and config, which the workspace holds.
fn load_project_for_run(
    file: &Path,
) -> Result<(Workspace, PathBuf, marrow_project::ProjectConfig), String> {
    let (workspace, _) = load_project(file, None)?;
    let project = workspace
        .project()
        .ok_or_else(|| "no project resolved for the file".to_string())?;
    let root = project.root.clone();
    let config = project.config.clone();
    Ok((workspace, root, config))
}

/// Evaluate one `entry` through Marrow's fresh-memory session under the locked
/// host, returning `{ value?, output, diagnostics: [] }` or a fault envelope.
fn run_entry(root: &Path, entry: Option<&str>) -> Json {
    let Some(entry) = entry else {
        return json!({
            "diagnostics": [{
                "message": "blocked-on-marrow entry string: run mode needs `entry`; Marrow resolves it through canonical function-entry facts, and this is not a stable production entry API"
            }],
            "output": ""
        });
    };
    let open = ProjectOpen::run()
        .with_entry_override(entry)
        .with_fresh_memory_store();
    let session = match ProjectSession::open(root, open) {
        Ok(session) => session,
        Err(error) => return project_session_error_json(&error, String::new()),
    };
    let host = locked_host();
    let mut output = String::new();
    match session.invoke(SessionEntry::new(entry, &host, &mut output)) {
        Ok(result) => run_output_json(result, output),
        Err(error) => project_invoke_error_json(&error, output),
    }
}

/// Discover and run the project's tests, each over its own fresh [`TreeStore`] under
/// the locked host, returning `{ output, diagnostics, tests: [...] }`. A test is a
/// public zero-parameter function in a test file; a `std::assert::*` failure is a
/// `failed` test, any other fault an `errored` test, mirroring `marrow test`.
fn run_tests(
    root: &Path,
    config: &marrow_project::ProjectConfig,
    program: &CheckedProgram,
) -> Json {
    let source_module_count = program.modules.len();
    let (report, combined) = match marrow_check::check_tests_program(root, config, program.clone())
    {
        Ok(result) => result,
        Err(error) => {
            return json!({ "diagnostics": [{ "code": error.code, "message": error.message }], "output": "", "tests": [] });
        }
    };
    let check_errors: Vec<Json> = report
        .diagnostics
        .iter()
        .filter(|d| d.severity == marrow_syntax::Severity::Error)
        .map(|d| json!({ "code": d.code, "message": d.message }))
        .collect();
    if !check_errors.is_empty() {
        return json!({ "diagnostics": check_errors, "output": "", "tests": [] });
    }

    let names: Vec<(String, PathBuf)> = combined.modules[source_module_count..]
        .iter()
        .flat_map(|module| {
            module
                .functions
                .iter()
                .filter(|f| f.public && f.params.is_empty())
                .map(|f| {
                    (
                        format!("{}::{}", module.name, f.name),
                        module.source_file.clone(),
                    )
                })
        })
        .collect();

    let runtime = combined.runtime();
    let host = locked_host();
    let mut tests = Vec::new();
    for (name, source_file) in &names {
        // Every test gets its own brand-new in-memory store, so one test cannot see
        // another's writes and none touches the project's real store.
        let store = TreeStore::memory();
        let entry = json!({ "name": name, "file": source_file.display().to_string() });
        let mut output = String::new();
        let result = match CheckedEntryCall::new(&runtime, name, Vec::new())
            .and_then(|call| run_entry_with_host(&store, &host, &call, &mut output))
        {
            Ok(_) => merge(entry, json!({ "outcome": "passed" })),
            Err(error) if error.code() == marrow_run::RUN_ASSERT => merge(
                entry,
                json!({ "outcome": "failed", "code": error.code(), "message": error.message, "line": error.span.line, "character": error.span.column }),
            ),
            Err(error) => merge(
                entry,
                json!({ "outcome": "errored", "code": error.code(), "message": error.message, "line": error.span.line, "character": error.span.column }),
            ),
        };
        tests.push(result);
    }
    json!({ "diagnostics": [], "output": "", "tests": tests })
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

/// A successful run's JSON: its returned value (when it returned one) and the
/// captured `print`/`write` output, truncated at [`OUTPUT_CAP`].
fn run_output_json(result: RunOutput, output: String) -> Json {
    let RunOutput { value } = result;
    json!({
        "value": value.map(value_to_json),
        "output": truncate(output),
        "diagnostics": [],
    })
}

/// A runtime fault's JSON: its stable `run.*` code, message, and source position.
/// A fault is reported in the result envelope (not as a transport error) so an
/// agent reads the same shape whether the run succeeded or faulted.
fn runtime_error_json(error: &RuntimeError, output: String) -> Json {
    json!({
        "diagnostics": [{
            "code": error.code(),
            "message": error.message,
            "line": error.span.line,
            "character": error.span.column,
        }],
        "output": truncate(output),
    })
}

fn project_invoke_error_json(error: &ProjectInvokeError, output: String) -> Json {
    match error {
        ProjectInvokeError::Runtime(error) => runtime_error_json(error, output),
        ProjectInvokeError::Session(error) => project_session_error_json(error, output),
    }
}

fn project_session_error_json(error: &ProjectSessionError, output: String) -> Json {
    json!({
        "diagnostics": [{
            "code": error.code(),
            "message": error.message(),
        }],
        "output": truncate(output),
    })
}

/// Truncate captured output to the cap with a marker, so a runaway program cannot
/// balloon the reply.
fn truncate(mut output: String) -> String {
    if output.len() > OUTPUT_CAP {
        let mut end = OUTPUT_CAP;
        while !output.is_char_boundary(end) {
            end -= 1;
        }
        output.truncate(end);
        output.push_str("\n…output truncated…");
    }
    output
}

/// A runtime [`Value`] as JSON for a run's returned value. Scalars keep their
/// JSON forms; structural values stay inspectable while sharing a small node
/// budget, so a presentation-only run cannot return an unbounded JSON tree.
fn value_to_json(value: Value) -> Json {
    let mut budget = ValueBudget::new(RUN_VALUE_NODE_CAP);
    value_to_json_bounded(value, &mut budget)
}

struct ValueBudget {
    remaining: usize,
}

impl ValueBudget {
    fn new(limit: usize) -> Self {
        Self { remaining: limit }
    }

    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            return false;
        }
        self.remaining -= 1;
        true
    }
}

fn value_to_json_bounded(value: Value, budget: &mut ValueBudget) -> Json {
    if !budget.take() {
        return json!({ "truncated": true });
    }
    match value {
        Value::Int(value) => json!(value),
        Value::Bool(value) => json!(value),
        Value::Str(value) => string_value_to_json(value),
        Value::Decimal(value) => json!(value.to_text()),
        Value::Date(days) => json!({ "date": days }),
        Value::Instant(nanos) => json!({ "instant": nanos.to_string() }),
        Value::Duration(nanos) => json!({ "duration": nanos.to_string() }),
        Value::Enum(value) => {
            json!({ "enum": { "id": value.enum_id().0, "member": value.member_id().0 } })
        }
        Value::Bytes(bytes) => json!({ "bytes": bytes.len() }),
        Value::Sequence(items) => sequence_to_json(items, budget),
        Value::LocalTree(entries) => json!({ "tree": entries.len() }),
        Value::Resource(fields) => resource_to_json(fields, budget),
        Value::Identity(identity) => identity_to_json(identity),
    }
}

struct BoundedString {
    value: String,
    truncated: bool,
    original_bytes: usize,
}

fn bounded_string(mut value: String) -> BoundedString {
    let original_bytes = value.len();
    let mut truncated = false;
    if original_bytes > RUN_VALUE_STRING_CAP {
        let mut end = RUN_VALUE_STRING_CAP;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        value.push('…');
        truncated = true;
    }
    BoundedString {
        value,
        truncated,
        original_bytes,
    }
}

fn string_value_to_json(value: String) -> Json {
    let value = bounded_string(value);
    if !value.truncated {
        return json!(value.value);
    }
    json!({
        "string": value.value,
        "truncated": true,
        "originalBytes": value.original_bytes,
    })
}

fn sequence_to_json(items: Vec<Value>, budget: &mut ValueBudget) -> Json {
    let total = items.len();
    let mut omitted = 0;
    let mut rendered = Vec::new();

    for item in items {
        if budget.remaining == 0 {
            omitted += 1;
            continue;
        }
        rendered.push(value_to_json_bounded(item, budget));
    }

    if omitted == 0 {
        Json::Array(rendered)
    } else {
        json!({
            "sequence": rendered,
            "truncated": true,
            "omitted": omitted,
            "total": total,
        })
    }
}

fn resource_to_json(fields: Vec<(String, Value)>, budget: &mut ValueBudget) -> Json {
    let total = fields.len();
    let mut omitted = 0;
    let mut field_names_truncated = false;
    let mut object = Map::new();
    let mut entries = Vec::new();

    for (name, value) in fields {
        if budget.remaining == 0 {
            omitted += 1;
            continue;
        }
        let name = bounded_string(name);
        field_names_truncated |= name.truncated;
        let value = value_to_json_bounded(value, budget);
        object.insert(name.value.clone(), value.clone());
        let mut entry = json!({
            "name": name.value,
            "value": value,
        });
        if name.truncated {
            entry["nameTruncated"] = json!(true);
            entry["nameOriginalBytes"] = json!(name.original_bytes);
        }
        entries.push(entry);
    }

    if omitted == 0 && !field_names_truncated {
        Json::Object(object)
    } else {
        json!({
            "resource": entries,
            "truncated": true,
            "omitted": omitted,
            "total": total,
        })
    }
}

fn identity_to_json(identity: marrow_run::IdentityValue) -> Json {
    let root = bounded_string(identity.root().to_string());
    let mut value = json!({
        "identity": {
            "root": root.value,
            "keyCount": identity.keys().len(),
        }
    });
    if root.truncated {
        value["identity"]["rootTruncated"] = json!(true);
        value["identity"]["rootOriginalBytes"] = json!(root.original_bytes);
    }
    value
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

/// The `.mw` spelling of a schema type, for the resource-schema projection.
fn type_name(ty: &Type) -> String {
    ty.to_string()
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
