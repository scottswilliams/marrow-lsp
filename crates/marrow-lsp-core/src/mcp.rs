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
//! - **Data access** ([`saved_roots`], [`saved_get`], [`saved_children`]) reads a
//!   project's real stored tree, so the transport gates it behind an explicit
//!   opt-in and passes `allow_data = false` otherwise; these functions then return
//!   a clear refusal envelope and never touch the store.
//! - **Execution** ([`run`]) evaluates a function over a *fresh* [`MemStore`] —
//!   never the project's real store — under a locked-down [`Host`] that grants
//!   only a deterministic clock and a captured log: no filesystem, no environment,
//!   no maintenance. An agent can confirm behavior without real side effects.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use marrow_check::{CheckedProgram, type_at};
use marrow_run::{Host, RunOutput, RuntimeError, Value, run_entry_with_host};
use marrow_schema::{IndexSchema, KeyDef, Node, NodeKind, ResourceSchema, SavedRootSchema, Type};
use marrow_store::mem::MemStore;
use serde_json::{Value as Json, json};

use crate::completion::completion;
use crate::diagnostics::path_to_url;
use crate::documents::Documents;
use crate::positions::Position;
use crate::store::StoreReader;
use crate::types::render_type;
use crate::workspace::Workspace;

/// A clamp on the saved bytes a `run`/test captures into its result. Output is a
/// program's own `print`/`write` stream; a runaway loop must not balloon the
/// reply, so it is truncated with a marker once it crosses this.
const OUTPUT_CAP: usize = 8 * 1024;

/// A clamp on the children one `saved_children` page returns to the agent, on top
/// of the [`StoreReader`]'s own cap — the reader already truncates, but the count
/// is named here so the MCP contract is explicit.
const CHILDREN_HINT: &str = "more children remained; narrow the path to page them";

const COMPLETION_MISSING_FACTS: &[&str] = &["canonical completion-context facts"];
const RESOURCE_SCHEMA_MISSING_FACTS: &[&str] = &[
    "catalog-bound resource/store/member identity",
    "presence/default facts",
    "typed protocol DTOs",
];
const SAVED_DATA_MISSING_FACTS: &[&str] = &[
    "catalog-bound saved-place identity",
    "typed children",
    "cursor/page facts",
    "snapshot/store generation",
    "stable data DTOs",
];
const DATA_INTEGRITY_MISSING_FACTS: &[&str] = &[
    "catalog/store identity",
    "store generation",
    "catalog epoch/digest",
    "typed repair or drift facts",
];
const RUN_MISSING_FACTS: &[&str] = &[
    "transitive effect facts",
    "durable-scope facts",
    "transaction facts",
    "runtime generation facts",
    "typed run protocol DTOs",
];

fn blocked_contract(description: &str, missing_facts: &[&str]) -> Json {
    json!({
        "status": "blocked-on-marrow",
        "stableProductionApi": false,
        "description": description,
        "missingFacts": missing_facts,
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
    blocked_contract("development helper", COMPLETION_MISSING_FACTS)
}

fn resource_schema_contract() -> Json {
    blocked_contract("development helper", RESOURCE_SCHEMA_MISSING_FACTS)
}

fn saved_data_contract() -> Json {
    blocked_contract("debug/admin prototype", SAVED_DATA_MISSING_FACTS)
}

fn data_integrity_contract() -> Json {
    presentation_contract("debug/admin advisory", DATA_INTEGRITY_MISSING_FACTS)
}

fn run_contract() -> Json {
    presentation_contract("debug/admin prototype", RUN_MISSING_FACTS)
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

/// `mw_resource_schema`: a JSON projection of one resource schema (by `name`) or
/// every resource in the project when `name` is omitted. The shape mirrors what an
/// agent needs to write correct saved paths: the resource's name, its saved root
/// (root + identity keys), its member tree (fields, keyed leaves, groups, nested),
/// and its indexes. Built from `ResourceSchema` — no schema logic is reinvented.
pub fn resource_schema(file: &Path, name: Option<&str>) -> Json {
    let contract = resource_schema_contract();
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => return with_contract(json!({ "resources": [], "error": error }), contract),
    };
    let Some(program) = workspace.program() else {
        return with_contract(json!({ "resources": [] }), contract);
    };
    let resources: Vec<Json> = all_resources(program)
        .filter(|resource| name.is_none_or(|name| resource.name == name))
        .map(schema_to_json)
        .collect();
    with_contract(json!({ "resources": resources }), contract)
}

/// Project one resource schema to JSON: name, optional saved root, member tree, and
/// indexes. The member tree is recursive (a group carries its nested members), so an
/// agent can navigate `^root(key).layer(key).field` from one document.
fn schema_to_json(resource: &ResourceSchema) -> Json {
    json!({
        "name": resource.name,
        "docs": resource.docs,
        "savedRoot": resource.saved_root.as_ref().map(saved_root_to_json),
        "members": resource.members.iter().map(node_to_json).collect::<Vec<_>>(),
        "indexes": resource.indexes.iter().map(index_to_json).collect::<Vec<_>>(),
    })
}

fn saved_root_to_json(saved: &SavedRootSchema) -> Json {
    json!({
        "root": saved.root,
        "identityKeys": saved.identity_keys.iter().map(key_def_to_json).collect::<Vec<_>>(),
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
/// short-lived [`StoreReader`]. Gated: with `allow_data = false` the function
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
    let reader = workspace.project().and_then(StoreReader::for_project);
    let result = serde_json::to_value(crate::data_explorer::saved_roots(reader.as_ref()))
        .unwrap_or_else(|_| json!({ "available": false, "roots": [] }));
    with_contract(result, contract)
}

/// `mw_saved_get`: the presence and schema-typed value at a saved `path`, read
/// through a [`StoreReader`]. The path mirrors the LSP Data Explorer's segment
/// encoding (root/key/field/layer/index/index_key). Gated like [`saved_roots`].
pub fn saved_get(file: &Path, path: Json, allow_data: bool) -> Json {
    let contract = saved_data_contract();
    if !allow_data {
        return data_disabled(contract);
    }
    let params: crate::data_explorer::SavedGetParams =
        match serde_json::from_value(json!({ "path": path })) {
            Ok(params) => params,
            Err(error) => {
                return with_contract(
                    json!({ "available": false, "error": format!("invalid path: {error}") }),
                    contract,
                );
            }
        };
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => {
            return with_contract(json!({ "available": false, "error": error }), contract);
        }
    };
    let reader = workspace.project().and_then(StoreReader::for_project);
    let empty = CheckedProgram::default();
    let program = workspace.program().unwrap_or(&empty);
    let result = serde_json::to_value(crate::data_explorer::saved_get(
        params,
        program,
        reader.as_ref(),
    ))
    .unwrap_or_else(|_| json!({ "available": false }));
    with_contract(result, contract)
}

/// `mw_saved_children`: the immediate children of a saved `path`, capped, read
/// through a [`StoreReader`]. Path encoding and gating match [`saved_get`].
pub fn saved_children(file: &Path, path: Json, allow_data: bool) -> Json {
    let contract = saved_data_contract();
    if !allow_data {
        return data_disabled(contract);
    }
    let params: crate::data_explorer::SavedChildrenParams =
        match serde_json::from_value(json!({ "path": path })) {
            Ok(params) => params,
            Err(error) => {
                return with_contract(
                    json!({ "available": false, "error": format!("invalid path: {error}") }),
                    contract,
                );
            }
        };
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => {
            return with_contract(json!({ "available": false, "error": error }), contract);
        }
    };
    let reader = workspace.project().and_then(StoreReader::for_project);
    let result = crate::data_explorer::saved_children(params, reader.as_ref());
    let mut value = serde_json::to_value(&result).unwrap_or_else(|_| json!({ "available": false }));
    if result.more
        && let Some(object) = value.as_object_mut()
    {
        object.insert("hint".to_string(), json!(CHILDREN_HINT));
    }
    with_contract(value, contract)
}

/// `mw_data_integrity`: the schema-change-impact advisory — a capped, on-demand
/// scan of the project's real stored data that flags every record the *current*
/// schema can no longer account for (an orphan path, or a value that no longer
/// decodes as its declared type). Reuses the same classification as
/// `marrow data integrity`, through a short-lived [`StoreReader`]. Gated like
/// [`saved_roots`]: with `allow_data = false` it returns a refusal envelope and
/// never opens the store. A project with no native store, or a store that cannot
/// be read right now, answers `available: false`.
pub fn data_integrity(file: &Path, allow_data: bool) -> Json {
    let contract = data_integrity_contract();
    if !allow_data {
        return data_disabled(contract);
    }
    let unavailable =
        json!({ "available": false, "findings": [], "scanned": 0, "truncated": false });
    let workspace = match load_project(file, None) {
        Ok((workspace, _)) => workspace,
        Err(error) => {
            let mut value = unavailable.clone();
            value["error"] = json!(error);
            return with_contract(value, contract);
        }
    };
    let reader = workspace.project().and_then(StoreReader::for_project);
    let empty = CheckedProgram::default();
    let program = workspace.program().unwrap_or(&empty);
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

/// Whether `mw_run` may decode positional JSON arguments with the debug/admin
/// scalar prototype.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunArgsPolicy {
    /// Reject non-empty run args until Marrow exposes typed run argument facts.
    StableTypedFactsOnly,
    /// Preserve the temporary scalar decoder behind an explicit opt-in.
    AllowPrototypeScalars,
}

/// `mw_run`: execute Marrow to confirm behavior — always sandboxed.
///
/// **Run mode** checks the project (`analyze_project` via the workspace), then
/// evaluates `entry` (`"module::fn"`) over a *fresh* [`MemStore`] under a
/// locked-down [`Host`]: a fixed clock and a captured log only — no filesystem, no
/// environment, no maintenance. Non-empty `args` are blocked unless
/// [`RunArgsPolicy::AllowPrototypeScalars`] is set, because Marrow does not yet
/// expose stable typed run argument facts. The result carries the returned
/// `value`, the captured `output`, and any check `diagnostics`.
///
/// **Test mode** runs `check_tests` then, for every public zero-parameter test
/// function, runs it over its *own* fresh [`MemStore`] under the same locked host,
/// reporting per-test pass/fail/error. `entry`/`args` are ignored.
///
/// The project's real store is never opened in either mode, so an agent can run
/// code with no risk to managed data.
pub fn run(
    file: &Path,
    entry: Option<&str>,
    args: &[Json],
    mode: RunMode,
    args_policy: RunArgsPolicy,
) -> Json {
    with_contract(
        run_result(file, entry, args, mode, args_policy),
        run_contract(),
    )
}

fn run_result(
    file: &Path,
    entry: Option<&str>,
    args: &[Json],
    mode: RunMode,
    args_policy: RunArgsPolicy,
) -> Json {
    if matches!(mode, RunMode::Run)
        && !args.is_empty()
        && args_policy == RunArgsPolicy::StableTypedFactsOnly
    {
        return prototype_args_refusal();
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
        RunMode::Run => run_entry(&snapshot.program, entry, args),
        RunMode::Test => run_tests(&root, &config, &snapshot.program),
    }
}

fn prototype_args_refusal() -> Json {
    json!({
        "diagnostics": [{
            "code": "mcp.run.prototype_args",
            "message": "mw_run args are blocked until typed run argument facts from Marrow are available; pass allowPrototypeArgs: true only for the debug/admin prototype scalar decoder"
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

/// Evaluate one `entry` over a fresh [`MemStore`] under the locked host, returning
/// `{ value?, output, diagnostics: [] }` or a runtime-fault envelope. The store is
/// brand new for this call and dropped when it returns; nothing persists.
fn run_entry(program: &CheckedProgram, entry: Option<&str>, args: &[Json]) -> Json {
    let Some(entry) = entry else {
        return json!({ "diagnostics": [{ "message": "run mode needs an `entry` of the form `module::fn`" }], "output": "" });
    };
    let arguments = match args
        .iter()
        .map(json_to_value)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(arguments) => arguments,
        Err(error) => return json!({ "diagnostics": [{ "message": error }], "output": "" }),
    };
    let store = RefCell::new(MemStore::new());
    let host = locked_host();
    match run_entry_with_host(program, &store, &host, entry, &arguments) {
        Ok(output) => run_output_json(output),
        Err(error) => runtime_error_json(&error),
    }
}

/// Discover and run the project's tests, each over its own fresh [`MemStore`] under
/// the locked host, returning `{ output, diagnostics, tests: [...] }`. A test is a
/// public zero-parameter function in a test file; a `std::assert::*` failure is a
/// `failed` test, any other fault an `errored` test, mirroring `marrow test`.
fn run_tests(
    root: &Path,
    config: &marrow_project::ProjectConfig,
    program: &CheckedProgram,
) -> Json {
    let (report, test_modules) = match marrow_check::check_tests(root, config, program) {
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

    let names: Vec<(String, PathBuf)> = test_modules
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

    // Resolve test names against the project plus the test modules, exactly as
    // `marrow test` does, so cross-module calls and resource constructors resolve.
    let mut combined = program.clone();
    combined.modules.extend(test_modules);

    let host = locked_host();
    let mut tests = Vec::new();
    for (name, source_file) in &names {
        // Every test gets its own brand-new in-memory store, so one test cannot see
        // another's writes and none touches the project's real store.
        let store = RefCell::new(MemStore::new());
        let entry = json!({ "name": name, "file": source_file.display().to_string() });
        let result = match run_entry_with_host(&combined, &store, &host, name, &[]) {
            Ok(_) => merge(entry, json!({ "outcome": "passed" })),
            Err(error) if error.code == marrow_run::RUN_ASSERT => merge(
                entry,
                json!({ "outcome": "failed", "code": error.code, "message": error.message, "line": error.span.line, "character": error.span.column }),
            ),
            Err(error) => merge(
                entry,
                json!({ "outcome": "errored", "code": error.code, "message": error.message, "line": error.span.line, "character": error.span.column }),
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
fn run_output_json(output: RunOutput) -> Json {
    let RunOutput { value, output } = output;
    json!({
        "value": value.map(value_to_json),
        "output": truncate(output),
        "diagnostics": [],
    })
}

/// A runtime fault's JSON: its stable `run.*` code, message, and source position.
/// A fault is reported in the result envelope (not as a transport error) so an
/// agent reads the same shape whether the run succeeded or faulted.
fn runtime_error_json(error: &RuntimeError) -> Json {
    json!({
        "diagnostics": [{
            "code": error.code,
            "message": error.message,
            "line": error.span.line,
            "character": error.span.column,
        }],
        "output": "",
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

/// A JSON scalar as a runtime [`Value`] for a positional `mw_run` argument. Only
/// the scalars an entry's parameters take are accepted — int, bool, string,
/// decimal-as-string — since a run argument is a scalar; anything else is rejected
/// with a message rather than coerced.
fn json_to_value(arg: &Json) -> Result<Value, String> {
    match arg {
        Json::Bool(value) => Ok(Value::Bool(*value)),
        Json::String(text) => Ok(Value::Str(text.clone())),
        Json::Number(number) => {
            if let Some(int) = number.as_i64() {
                Ok(Value::Int(int))
            } else {
                Err(format!(
                    "the run argument `{number}` is not an integer; pass a decimal as a string"
                ))
            }
        }
        other => Err(format!(
            "a run argument must be an int, bool, or string scalar, not `{other}`"
        )),
    }
}

/// A runtime [`Value`] as JSON for a run's returned value. The scalars render to
/// their JSON forms; the wide and structural values (instant, duration, sequence,
/// resource, identity) render to a tagged string so the result stays inspectable
/// without inventing a numeric form JSON cannot hold.
fn value_to_json(value: Value) -> Json {
    match value {
        Value::Int(value) => json!(value),
        Value::Bool(value) => json!(value),
        Value::Str(value) => json!(value),
        Value::Decimal(value) => json!(value.to_text()),
        Value::Date(days) => json!({ "date": days }),
        Value::Instant(nanos) => json!({ "instant": nanos.to_string() }),
        Value::Duration(nanos) => json!({ "duration": nanos.to_string() }),
        Value::Bytes(bytes) => json!({ "bytes": bytes.len() }),
        Value::Sequence(items) => json!(items.into_iter().map(value_to_json).collect::<Vec<_>>()),
        Value::LocalTree(entries) => json!({ "tree": entries.len() }),
        Value::Resource(fields) => Json::Object(
            fields
                .into_iter()
                .map(|(name, value)| (name, value_to_json(value)))
                .collect(),
        ),
        Value::Identity(segments) => json!({ "identity": segments.len() }),
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

/// Every resource declared across the program's modules.
fn all_resources(program: &CheckedProgram) -> impl Iterator<Item = &ResourceSchema> {
    program
        .modules
        .iter()
        .flat_map(|module| module.resources.iter())
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
mod tests {
    use super::*;
    use marrow_store::backend::Backend;
    use marrow_store::path::{PathSegment, SavedKey, encode_path};
    use marrow_store::redb::RedbStore;

    fn assert_contract(result: &Json, status: &str, description: &str, missing_facts: &[&str]) {
        let contract = &result["contract"];
        assert_eq!(contract["status"], status, "contract status for {result}");
        assert_eq!(
            contract["stableProductionApi"], false,
            "stableProductionApi must be false for {result}"
        );
        assert_eq!(
            contract["description"], description,
            "contract description for {result}"
        );
        let actual: Vec<&str> = contract["missingFacts"]
            .as_array()
            .unwrap_or_else(|| panic!("contract missingFacts must be an array: {result}"))
            .iter()
            .map(|fact| fact.as_str().expect("missing fact string"))
            .collect();
        assert_eq!(actual, missing_facts, "missing facts for {result}");
    }

    /// Write a clean two-resource project to a temp dir and return its root and a
    /// file inside it. The project declares a saved `Book` resource and a couple of
    /// functions an `mw_run`/`mw_type_at` test can target.
    fn project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" }, "tests": ["tests/**/*.mw"] }"#,
        )
        .unwrap();
        let src = root.join("src/shelf");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("books.mw"),
            "\
module shelf::books

resource Book at ^books(id: int)
    required title: string
    tags(pos: int): string

    index byTitle(title, id)

pub fn double(n: int): int
    return n * 2

pub fn greet(name: string): string
    return $\"hi {name}\"

pub fn shout()
    print(\"loud\")
",
        )
        .unwrap();
        let file = src.join("books.mw");
        (dir, file)
    }

    #[test]
    fn check_reports_a_return_type_error_for_a_project_overlay() {
        let (_dir, file) = project();
        let overlay = "module shelf::books\n\npub fn answer(): int\n    return \"nope\"\n";
        let result = check(Some(&file), Some(overlay));
        let diagnostics = result["diagnostics"].as_array().unwrap();
        assert!(
            diagnostics.iter().any(|d| d["code"] == "check.return_type"),
            "expected a check.return_type diagnostic, got {result}"
        );
    }

    #[test]
    fn check_of_a_clean_file_has_no_diagnostics() {
        let (_dir, file) = project();
        let result = check(Some(&file), None);
        assert_eq!(
            result["diagnostics"].as_array().unwrap().len(),
            0,
            "{result}"
        );
    }

    #[test]
    fn check_snippet_reports_a_syntax_error() {
        // A bare snippet with a broken declaration: no project, parser diagnostics.
        let result = check(None, Some("module a\n\npub fn broken(:\n"));
        assert!(
            !result["diagnostics"].as_array().unwrap().is_empty(),
            "a malformed snippet should report at least one diagnostic, got {result}"
        );
    }

    #[test]
    fn type_at_reports_the_type_of_a_parameter_use() {
        let (_dir, file) = project();
        // `    return n * 2` is line 9 (zero-based); `    return ` is 11 chars, so
        // the `n` parameter use sits at character 11.
        let result = type_at_position(&file, 9, 11);
        assert_eq!(
            result["type"], "int",
            "the `n` parameter use is an int: {result}"
        );
    }

    #[test]
    fn complete_in_a_function_body_lists_locals_and_keywords() {
        let (_dir, file) = project();
        // A bare position inside `double`'s body (line 9, after `    return `) lists
        // in-scope names and keywords — proving the schema-backed program loaded and
        // the classifier ran against the file's own text.
        let result = complete(&file, 9, 11);
        let items = result["items"].as_array().unwrap();
        assert!(
            !items.is_empty(),
            "completion should offer items, got {result}"
        );
        assert!(
            items
                .iter()
                .any(|item| item["label"] == "n" && item["kind"] == "variable"),
            "the `n` parameter should be an in-scope completion, got {result}"
        );
        assert!(
            items.iter().any(|item| item["kind"] == "keyword"),
            "a bare position should offer keywords, got {result}"
        );
        assert_contract(
            &result,
            "blocked-on-marrow",
            "development helper",
            &["canonical completion-context facts"],
        );
    }

    #[test]
    fn resource_schema_shapes_the_book_resource() {
        let (_dir, file) = project();
        let result = resource_schema(&file, Some("Book"));
        let resources = result["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 1, "one Book resource: {result}");
        let book = &resources[0];
        assert_eq!(book["name"], "Book");
        assert_eq!(book["savedRoot"]["root"], "books");
        assert_eq!(book["savedRoot"]["identityKeys"][0]["name"], "id");
        assert_eq!(book["savedRoot"]["identityKeys"][0]["type"], "int");
        // `title` is a required string field; `tags` is a keyed leaf; `byTitle` an index.
        let members = book["members"].as_array().unwrap();
        assert!(
            members
                .iter()
                .any(|m| m["name"] == "title" && m["kind"] == "field" && m["type"] == "string")
        );
        assert!(
            members
                .iter()
                .any(|m| m["name"] == "tags" && m["kind"] == "leaf")
        );
        assert!(
            book["indexes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["name"] == "byTitle")
        );
        assert_contract(
            &result,
            "blocked-on-marrow",
            "development helper",
            &[
                "catalog-bound resource/store/member identity",
                "presence/default facts",
                "typed protocol DTOs",
            ],
        );
    }

    #[test]
    fn resource_schema_without_a_name_lists_every_resource() {
        let (_dir, file) = project();
        let result = resource_schema(&file, None);
        assert_eq!(result["resources"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn run_executes_a_pure_function_and_returns_its_value() {
        let (_dir, file) = project();
        let result = run(
            &file,
            Some("shelf::books::double"),
            &[json!(21)],
            RunMode::Run,
            RunArgsPolicy::AllowPrototypeScalars,
        );
        assert_eq!(result["value"], 42, "double(21) == 42: {result}");
        assert_eq!(result["diagnostics"].as_array().unwrap().len(), 0);
        assert_contract(
            &result,
            "presentation-only",
            "debug/admin prototype",
            &[
                "transitive effect facts",
                "durable-scope facts",
                "transaction facts",
                "runtime generation facts",
                "typed run protocol DTOs",
            ],
        );
    }

    #[test]
    fn run_blocks_non_empty_args_without_prototype_policy_before_loading() {
        let result = run(
            Path::new("/nope/project/src/main.mw"),
            Some("app::main"),
            &[json!(1)],
            RunMode::Run,
            RunArgsPolicy::StableTypedFactsOnly,
        );
        let message = result["diagnostics"][0]["message"].as_str().unwrap();
        assert!(
            message.contains("typed run argument facts from Marrow"),
            "non-empty run args should be blocked before project loading: {result}"
        );
        assert_eq!(result["output"], "");
        assert_contract(
            &result,
            "presentation-only",
            "debug/admin prototype",
            &[
                "transitive effect facts",
                "durable-scope facts",
                "transaction facts",
                "runtime generation facts",
                "typed run protocol DTOs",
            ],
        );
    }

    #[test]
    fn run_value_json_summarizes_local_trees() {
        assert_eq!(
            value_to_json(Value::LocalTree(Vec::new())),
            json!({ "tree": 0 })
        );
    }

    #[test]
    fn run_captures_print_output() {
        let (_dir, file) = project();
        let result = run(
            &file,
            Some("shelf::books::shout"),
            &[],
            RunMode::Run,
            RunArgsPolicy::StableTypedFactsOnly,
        );
        assert!(
            result["output"].as_str().unwrap().contains("loud"),
            "print output should be captured, got {result}"
        );
    }

    #[test]
    fn run_executes_shelf_fixture_main() {
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/shelf/src/shelf/sample.mw")
            .canonicalize()
            .expect("the shelf fixture exists");
        let result = run(
            &file,
            Some("shelf::sample::main"),
            &[],
            RunMode::Run,
            RunArgsPolicy::StableTypedFactsOnly,
        );
        assert_eq!(result["diagnostics"], json!([]), "{result}");
        assert!(
            result["output"].as_str().unwrap().contains("Small Gods"),
            "the shelf fixture should print its seeded fiction book, got {result}"
        );
    }

    #[test]
    fn output_truncation_preserves_utf8_boundaries() {
        let output = "€".repeat(OUTPUT_CAP);
        let truncated = truncate(output);

        assert!(truncated.ends_with("…output truncated…"));
    }

    #[test]
    fn run_passes_a_string_argument() {
        let (_dir, file) = project();
        let result = run(
            &file,
            Some("shelf::books::greet"),
            &[json!("Ada")],
            RunMode::Run,
            RunArgsPolicy::AllowPrototypeScalars,
        );
        assert_eq!(result["value"], "hi Ada", "{result}");
    }

    #[test]
    fn run_does_not_touch_the_real_store() {
        // The project pins a native store; a run that would write must use a fresh
        // MemStore, so the on-disk store file is never created by a run.
        let (dir, file) = project();
        let _ = run(
            &file,
            Some("shelf::books::double"),
            &[json!(1)],
            RunMode::Run,
            RunArgsPolicy::AllowPrototypeScalars,
        );
        let store_file = dir.path().join("data").join("marrow.redb");
        assert!(
            !store_file.exists(),
            "a sandboxed run must never create or open the project store"
        );
    }

    #[test]
    fn test_mode_runs_each_test_over_a_fresh_store() {
        let (dir, file) = project();
        let tests_dir = dir.path().join("tests");
        std::fs::create_dir_all(&tests_dir).unwrap();
        std::fs::write(
            tests_dir.join("books_test.mw"),
            "\
use shelf::books

pub fn doubles()
    std::assert::isTrue(books::double(2) == 4)

pub fn fails()
    std::assert::isTrue(books::double(2) == 5)
",
        )
        .unwrap();
        let result = run(
            &file,
            None,
            &[],
            RunMode::Test,
            RunArgsPolicy::StableTypedFactsOnly,
        );
        let tests = result["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 2, "two test functions discovered: {result}");
        let doubles = tests
            .iter()
            .find(|t| t["name"].as_str().unwrap().ends_with("doubles"))
            .unwrap();
        assert_eq!(doubles["outcome"], "passed");
        let fails = tests
            .iter()
            .find(|t| t["name"].as_str().unwrap().ends_with("fails"))
            .unwrap();
        assert_eq!(
            fails["outcome"], "failed",
            "an assertion failure is a failed test: {fails}"
        );
        assert_contract(
            &result,
            "presentation-only",
            "debug/admin prototype",
            &[
                "transitive effect facts",
                "durable-scope facts",
                "transaction facts",
                "runtime generation facts",
                "typed run protocol DTOs",
            ],
        );
    }

    #[test]
    fn run_test_mode_ignores_args_without_prototype_policy() {
        let (_dir, file) = project();
        let result = run(
            &file,
            None,
            &[json!(1)],
            RunMode::Test,
            RunArgsPolicy::StableTypedFactsOnly,
        );
        assert_eq!(result["diagnostics"], json!([]), "{result}");
        assert!(
            result["tests"].as_array().is_some(),
            "test mode should remain on the test path even when args are present: {result}"
        );
    }

    #[test]
    fn data_tools_refuse_when_not_enabled() {
        let (_dir, file) = project();
        let roots = saved_roots(&file, false);
        assert_eq!(roots["available"], false);
        assert_eq!(roots["dataAccess"], "disabled");
        assert_contract(
            &roots,
            "blocked-on-marrow",
            "debug/admin prototype",
            &[
                "catalog-bound saved-place identity",
                "typed children",
                "cursor/page facts",
                "snapshot/store generation",
                "stable data DTOs",
            ],
        );
        let got = saved_get(&file, json!([{ "root": "books" }]), false);
        assert_eq!(got["dataAccess"], "disabled");
        assert_contract(
            &got,
            "blocked-on-marrow",
            "debug/admin prototype",
            &[
                "catalog-bound saved-place identity",
                "typed children",
                "cursor/page facts",
                "snapshot/store generation",
                "stable data DTOs",
            ],
        );
        let children = saved_children(&file, json!([]), false);
        assert_eq!(children["dataAccess"], "disabled");
        assert_contract(
            &children,
            "blocked-on-marrow",
            "debug/admin prototype",
            &[
                "catalog-bound saved-place identity",
                "typed children",
                "cursor/page facts",
                "snapshot/store generation",
                "stable data DTOs",
            ],
        );
        let integrity = data_integrity(&file, false);
        assert_eq!(integrity["dataAccess"], "disabled");
        assert_contract(
            &integrity,
            "presentation-only",
            "debug/admin advisory",
            &[
                "catalog/store identity",
                "store generation",
                "catalog epoch/digest",
                "typed repair or drift facts",
            ],
        );
    }

    #[test]
    fn data_integrity_flags_an_orphan_under_an_undeclared_field() {
        let (dir, file) = project();
        // The project declares `^books(id).title`; seed an extra `^books(1).sticker`
        // the schema does not declare, so the scan flags it as an orphan.
        let store_path = dir.path().join("data").join("marrow.redb");
        std::fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        {
            let mut store = RedbStore::open(&store_path).unwrap();
            let title = encode_path(&[
                PathSegment::Root("books".to_string()),
                PathSegment::RecordKey(SavedKey::Int(1)),
                PathSegment::Field("title".to_string()),
            ]);
            store.write(&title, b"Mort".to_vec()).unwrap();
            let orphan = encode_path(&[
                PathSegment::Root("books".to_string()),
                PathSegment::RecordKey(SavedKey::Int(1)),
                PathSegment::Field("sticker".to_string()),
            ]);
            store.write(&orphan, b"gold".to_vec()).unwrap();
        }
        let result = data_integrity(&file, true);
        assert_eq!(result["available"], true, "{result}");
        let findings = result["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1, "one orphan finding: {result}");
        assert_eq!(findings[0]["kind"], "orphan");
        assert_eq!(findings[0]["path"], "^books(1).sticker");
        assert_eq!(result["truncated"], false);
    }

    #[test]
    fn data_integrity_is_clean_on_a_matching_store() {
        let (dir, file) = project();
        let store_path = dir.path().join("data").join("marrow.redb");
        std::fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        {
            let mut store = RedbStore::open(&store_path).unwrap();
            let title = encode_path(&[
                PathSegment::Root("books".to_string()),
                PathSegment::RecordKey(SavedKey::Int(1)),
                PathSegment::Field("title".to_string()),
            ]);
            store.write(&title, b"Mort".to_vec()).unwrap();
        }
        let result = data_integrity(&file, true);
        assert_eq!(result["available"], true, "{result}");
        assert!(
            result["findings"].as_array().unwrap().is_empty(),
            "{result}"
        );
    }

    #[test]
    fn data_tools_answer_when_enabled_against_a_seeded_store() {
        let (dir, file) = project();
        // Seed `^books(1).title = "Mort"` into the native store the project pins.
        let store_path = dir.path().join("data").join("marrow.redb");
        std::fs::create_dir_all(store_path.parent().unwrap()).unwrap();
        {
            let mut store = RedbStore::open(&store_path).unwrap();
            let title = encode_path(&[
                PathSegment::Root("books".to_string()),
                PathSegment::RecordKey(SavedKey::Int(1)),
                PathSegment::Field("title".to_string()),
            ]);
            store.write(&title, b"Mort".to_vec()).unwrap();
        }

        let roots = saved_roots(&file, true);
        assert_eq!(
            roots["available"], true,
            "seeded store is readable: {roots}"
        );
        assert!(
            roots["roots"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r == "books")
        );

        let got = saved_get(
            &file,
            json!([{ "root": "books" }, { "key": { "int": 1 } }, { "field": "title" }]),
            true,
        );
        assert_eq!(got["available"], true, "{got}");
        assert_eq!(got["value"], "\"Mort\"");
        assert_eq!(got["type"], "string");

        let children = saved_children(&file, json!([{ "root": "books" }]), true);
        assert_eq!(children["available"], true, "{children}");
        assert_eq!(children["children"][0]["key"]["int"], 1);
    }
}
