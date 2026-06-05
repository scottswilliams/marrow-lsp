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
//! - **Execution** ([`run`]) evaluates a function over a *fresh* [`TreeStore`] —
//!   never the project's real store — under a locked-down [`Host`] that grants
//!   only a deterministic clock and a captured log: no filesystem, no environment,
//!   no maintenance. Projects that still need a baseline catalog identity return a
//!   blocker instead of letting this tool write durable state.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use marrow_check::{CheckedProgram, type_at};
use marrow_run::{CheckedEntryCall, Host, RunOutput, RuntimeError, Value, run_entry_with_host};
use marrow_schema::{IndexSchema, KeyDef, Node, NodeKind, ResourceSchema, StoreSchema, Type};
use marrow_store::tree::TreeStore;
use serde_json::{Value as Json, json};

use crate::completion::completion;
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
    presentation_contract("root-only data helper", SAVED_DATA_MISSING_FACTS)
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
/// evaluates `entry` (`"module::fn"`) over a *fresh* [`TreeStore`] under a
/// locked-down [`Host`]: a fixed clock and a captured log only — no filesystem, no
/// environment, no maintenance. Non-empty `args` are blocked because Marrow does
/// not yet expose stable typed run argument facts. The result carries the returned
/// `value`, the captured `output`, and any check `diagnostics`.
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

    let program = &snapshot.program;
    if crate::catalog_admission::requires_accepted_catalog_identity(program) {
        return run_catalog_refusal();
    }

    match mode {
        RunMode::Run => run_entry(program, entry),
        RunMode::Test => run_tests(&root, &config, program),
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

fn run_catalog_refusal() -> Json {
    json!({
        "diagnostics": [{
            "code": "mcp.run.catalog",
            "message": "blocked-on-marrow: mw_run will not establish accepted catalog identity; use Marrow's production catalog flow or wait for read-only run admission facts"
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

/// Evaluate one `entry` over a fresh [`TreeStore`] under the locked host, returning
/// `{ value?, output, diagnostics: [] }` or a runtime-fault envelope. The store is
/// brand new for this call and dropped when it returns; nothing persists.
fn run_entry(program: &CheckedProgram, entry: Option<&str>) -> Json {
    let Some(entry) = entry else {
        return json!({
            "diagnostics": [{
                "message": "blocked-on-marrow entry string: run mode needs `entry`; Marrow resolves it through canonical function-entry facts, and this is not a stable production entry API"
            }],
            "output": ""
        });
    };
    let store = TreeStore::memory();
    let host = locked_host();
    let runtime = program.runtime();
    let call = match CheckedEntryCall::new(&runtime, entry, Vec::new()) {
        Ok(call) => call,
        Err(error) => return runtime_error_json(&error),
    };
    match run_entry_with_host(&store, &host, &call) {
        Ok(output) => run_output_json(output),
        Err(error) => runtime_error_json(&error),
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
    let (report, combined) = match marrow_check::check_tests_program(root, config, program) {
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
        let result = match CheckedEntryCall::new(&runtime, name, Vec::new())
            .and_then(|call| run_entry_with_host(&store, &host, &call))
        {
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
        Value::Enum(value) => {
            json!({ "enum": { "id": value.enum_id().0, "member": value.member_id().0 } })
        }
        Value::Bytes(bytes) => json!({ "bytes": bytes.len() }),
        Value::Sequence(items) => json!(items.into_iter().map(value_to_json).collect::<Vec<_>>()),
        Value::LocalTree(entries) => json!({ "tree": entries.len() }),
        Value::Resource(fields) => Json::Object(
            fields
                .into_iter()
                .map(|(name, value)| (name, value_to_json(value)))
                .collect(),
        ),
        Value::Identity(identity) => {
            json!({ "identity": { "root": identity.root(), "keyCount": identity.keys().len() } })
        }
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

resource Book
    required title: string
    tags(pos: int): string

store ^books(id: int): Book
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

    fn pure_project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "tests": ["tests/**/*.mw"] }"#,
        )
        .unwrap();
        let src = root.join("src/shelf");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("books.mw"),
            "\
module shelf::books

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
        // `    return n * 2` is line 10 (zero-based); `    return ` is 11 chars, so
        // the `n` parameter use sits at character 11.
        let result = type_at_position(&file, 10, 11);
        assert_eq!(
            result["type"], "int",
            "the `n` parameter use is an int: {result}"
        );
    }

    #[test]
    fn complete_in_a_function_body_lists_locals_and_keywords() {
        let (_dir, file) = project();
        // A bare position inside `double`'s body (line 10, after `    return `) lists
        // in-scope names and keywords — proving the schema-backed program loaded and
        // the classifier ran against the file's own text.
        let result = complete(&file, 10, 11);
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
            "presentation-only",
            "development helper",
            &["canonical completion-context facts"],
        );
    }

    #[test]
    fn resource_schema_shapes_the_book_resource() {
        let (_dir, file) = project();
        let result = resource_schema(&file, "Book");
        let resources = result["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 1, "one Book resource: {result}");
        let book = &resources[0];
        assert_eq!(book["name"], "Book");
        let stores = book["stores"].as_array().unwrap();
        assert_eq!(stores.len(), 1);
        let store = &stores[0];
        assert_eq!(store["root"], "books");
        assert_eq!(store["resource"], "Book");
        assert_eq!(store["identityKeys"][0]["name"], "id");
        assert_eq!(store["identityKeys"][0]["type"], "int");
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
            store["indexes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["name"] == "byTitle")
        );
        assert_contract(
            &result,
            "presentation-only",
            "development helper",
            &[
                "catalog-bound resource/store/member identity",
                "presence/default facts",
                "typed protocol DTOs",
            ],
        );
    }

    #[test]
    fn resource_schema_refuses_ambiguous_resource_names() {
        let (dir, file) = project();
        let other = dir.path().join("src/other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            other.join("books.mw"),
            "\
module other::books

resource Book
    required code: string
",
        )
        .unwrap();

        let result = resource_schema(&file, "Book");
        assert_eq!(result["resources"], json!([]), "{result}");
        assert_eq!(
            result["diagnostics"][0]["code"], "mcp.resourceSchema.identity",
            "{result}"
        );
        assert_contract(
            &result,
            "presentation-only",
            "development helper",
            &[
                "catalog-bound resource/store/member identity",
                "presence/default facts",
                "typed protocol DTOs",
            ],
        );
    }

    #[test]
    fn run_executes_a_pure_function_and_returns_its_value() {
        let (_dir, file) = pure_project();
        let result = run(&file, Some("shelf::books::shout"), &[], RunMode::Run);
        assert_eq!(result["diagnostics"].as_array().unwrap().len(), 0);
        assert!(
            result["output"].as_str().unwrap().contains("loud"),
            "zero-argument run should execute: {result}"
        );
        assert_contract(
            &result,
            "presentation-only",
            "sandboxed execution helper",
            &[
                "canonical function-entry facts",
                "transitive effect facts",
                "durable-scope facts",
                "transaction facts",
                "runtime generation facts",
                "typed run protocol DTOs",
            ],
        );
    }

    #[test]
    fn run_blocks_non_empty_args_before_loading() {
        let result = run(
            Path::new("/nope/project/src/main.mw"),
            Some("app::main"),
            &[json!(1)],
            RunMode::Run,
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
            "sandboxed execution helper",
            &[
                "canonical function-entry facts",
                "transitive effect facts",
                "durable-scope facts",
                "transaction facts",
                "runtime generation facts",
                "typed run protocol DTOs",
            ],
        );
    }

    #[test]
    fn run_without_entry_frames_the_blocked_entry_contract() {
        let (_dir, file) = pure_project();
        let result = run(&file, None, &[], RunMode::Run);
        let message = result["diagnostics"][0]["message"].as_str().unwrap();
        assert!(
            message.contains("blocked-on-marrow entry string"),
            "missing entry should frame entry as blocked: {result}"
        );
        assert!(
            message.contains("canonical function-entry facts"),
            "missing entry should name the blocking facts: {result}"
        );
        assert!(
            message.contains("not a stable production entry API"),
            "missing entry should not imply a stable entry API: {result}"
        );
        assert_eq!(result["output"], "");
        assert_contract(
            &result,
            "presentation-only",
            "sandboxed execution helper",
            &[
                "canonical function-entry facts",
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
    fn run_contract_marks_entry_string_as_presentation_only() {
        let (_dir, file) = pure_project();
        let result = run(&file, Some("shelf::books::shout"), &[], RunMode::Run);
        assert_eq!(result["diagnostics"], json!([]), "{result}");
        assert!(
            result["output"].as_str().unwrap().contains("loud"),
            "a zero-argument explicit entry should still run, got {result}"
        );
        assert_contract(
            &result,
            "presentation-only",
            "sandboxed execution helper",
            &[
                "canonical function-entry facts",
                "transitive effect facts",
                "durable-scope facts",
                "transaction facts",
                "runtime generation facts",
                "typed run protocol DTOs",
            ],
        );
    }

    #[test]
    fn run_blocks_shelf_fixture_until_catalog_identity_is_established() {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/shelf")
            .canonicalize()
            .expect("the shelf fixture exists");
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src/shelf");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::copy(fixture.join("marrow.json"), dir.path().join("marrow.json")).unwrap();
        std::fs::copy(fixture.join("src/shelf/sample.mw"), src.join("sample.mw")).unwrap();
        let file = src.join("sample.mw");
        let result = run(&file, Some("shelf::sample::main"), &[], RunMode::Run);
        assert_eq!(
            result["diagnostics"][0]["code"], "mcp.run.catalog",
            "{result}"
        );
        assert!(
            result["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("accepted catalog identity"),
            "durable fixture should block on catalog identity, got {result}"
        );
        assert!(
            !dir.path().join("marrow.catalog.json").exists(),
            "mw_run must not establish the fixture's accepted catalog"
        );
    }

    #[test]
    fn output_truncation_preserves_utf8_boundaries() {
        let output = "€".repeat(OUTPUT_CAP);
        let truncated = truncate(output);

        assert!(truncated.ends_with("…output truncated…"));
    }

    #[test]
    fn run_blocks_string_arguments() {
        let (_dir, file) = pure_project();
        let result = run(
            &file,
            Some("shelf::books::greet"),
            &[json!("Ada")],
            RunMode::Run,
        );
        let message = result["diagnostics"][0]["message"].as_str().unwrap();
        assert!(
            message.contains("typed run argument facts from Marrow"),
            "string args should be blocked: {result}"
        );
    }

    #[test]
    fn run_does_not_touch_the_real_store() {
        // The project pins a native store and has no accepted catalog yet. A
        // presentation-only MCP run must not establish durable project state.
        let (dir, file) = project();
        let result = run(&file, Some("shelf::books::shout"), &[], RunMode::Run);
        assert_eq!(
            result["diagnostics"][0]["code"], "mcp.run.catalog",
            "{result}"
        );
        let store_file = dir.path().join("data").join("marrow.redb");
        assert!(
            !store_file.exists(),
            "a sandboxed run must never create or open the project store"
        );
        let catalog_file = dir.path().join("marrow.catalog.json");
        assert!(
            !catalog_file.exists(),
            "a presentation-only run must not write the accepted catalog"
        );
    }

    #[test]
    fn test_mode_runs_each_test_over_a_fresh_store() {
        let (dir, file) = pure_project();
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
        let result = run(&file, None, &[], RunMode::Test);
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
            "sandboxed execution helper",
            &[
                "canonical function-entry facts",
                "transitive effect facts",
                "durable-scope facts",
                "transaction facts",
                "runtime generation facts",
                "typed run protocol DTOs",
            ],
        );
    }

    #[test]
    fn run_test_mode_blocks_args() {
        let (_dir, file) = pure_project();
        let result = run(&file, None, &[json!(1)], RunMode::Test);
        assert_eq!(
            result["diagnostics"][0]["code"], "mcp.run.args",
            "test mode args should be blocked before running tests: {result}"
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
            "presentation-only",
            "root-only data helper",
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
}
