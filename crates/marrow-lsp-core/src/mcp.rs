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
//! - **Data access** reads a project's real stored tree. Reads
//!   ([`saved_roots`], [`surface_read`], data children/read) take a [`ReadAccess`]
//!   grant and writes ([`surface_write`]) take a distinct [`WriteAccess`] grant, so
//!   read and write are opted into separately and a read-only session cannot reach
//!   the writable executor. Without the matching grant the function returns a clear
//!   refusal envelope and never touches the store; every granted write appends a
//!   record to the project's append-only audit trail. [`surface_routes`] reads only
//!   source/catalog facts and stays outside this gate.
//! - **Execution** ([`run`]) evaluates a function through Marrow's fresh-memory
//!   project session — never the project's real store — under a locked-down
//!   [`Host`] that grants only a deterministic clock and a captured log: no
//!   filesystem, no environment, no maintenance.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use marrow_check::{
    tooling::{
        SOURCE_COMPLETION_PROFILE_VERSION, SourceCompletionItemKind,
        SourceEnumNamespaceCompletionFact, SourceModuleNamespaceCompletionFact,
        SourceNamespaceCompletionFact, SourceNamespaceEnumMemberStatus,
        SourceNamespaceFunctionCompletion, source_completion_fact,
        source_namespace_completion_file_fact,
    },
    type_at,
};
use marrow_json::resource_schema::{
    RESOURCE_SCHEMA_PROFILE_VERSION, resource_schema_for_name as marrow_resource_schema_for_name,
};
use marrow_json::surface::{
    SURFACE_OPERATION_PROFILE_VERSION, SURFACE_ROUTE_PROFILE_VERSION, SurfaceAbiJson,
    SurfaceIdentityJson, SurfaceOperationKind, SurfaceOperationRequestBodyJson,
    SurfaceOperationRequestJson, SurfaceRouteManifestJson, execute_project_surface_operation,
    execute_project_surface_operation_read_only,
};
use marrow_run::{
    CheckedEntryCall, EntryArgument, EntryDescriptor, EntryInvocation, Host, ProjectInvokeError,
    ProjectOpen, ProjectSession, ProjectSessionError, ProjectSurfaceReadSession,
    ProjectSurfaceSession, SessionEntry, StoreStamp,
};
use serde_json::{Value as Json, json};

use crate::data_explorer::{
    DataChildrenRequest, DataReadRequest, session_data_child_views_page, session_data_read,
};
use crate::diagnostics::path_to_url;
use crate::documents::Documents;
use crate::execution::execution_boundary_json;
use crate::positions::Position;
use crate::store::{SavedDataSession, open_saved_data_session};
use crate::types::render_type;
use marrow_project::{discover_modules, discover_test_modules};

use crate::workspace::{COMMITTED_LOCK_FILE, Project, Workspace};

const SOURCE_NAMESPACE_COMPLETION_PROFILE_VERSION: &str = "source.namespace.completion.v1";

/// The stable shape version an agent pins to detect a breaking rename of a tool's
/// top-level result. The fact-bearing tools carry their Marrow fact's version;
/// these name the shapes this crate owns (`mw_check`, `mw_type_at`, `mw_run`),
/// which have no Marrow fact version of their own, so the crate versions them here.
pub const CHECK_PROFILE_VERSION: &str = "mcp.check.v1";
pub const TYPE_AT_PROFILE_VERSION: &str = "mcp.type_at.v1";
pub const RUN_PROFILE_VERSION: &str = "mcp.run.v1";

/// Stamp a tool result with its shape version, so an agent pinning the shape can
/// detect a breaking rename. Left in place if the object already carries one.
fn with_profile_version(mut result: Json, version: &str) -> Json {
    if let Some(object) = result.as_object_mut() {
        object
            .entry("profile_version")
            .or_insert_with(|| json!(version));
    }
    result
}

fn production_contract(description: &str) -> Json {
    json!({
        "status": "ready",
        "stableProductionApi": true,
        "description": description,
        "missingFacts": [],
    })
}

fn completion_contract() -> Json {
    let mut contract = production_contract("source completion fact");
    contract["basis"] = json!("Marrow source completion fact");
    contract
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
    // The write opt-in is a separate gate from read data access: a read-only
    // session cannot reach this executor, so the contract names the two grants
    // apart rather than folding them into one "data access" bit.
    contract["writeOptIn"] = json!("gated");
    contract["operations"] = json!("read/update/action");
    contract
}

pub fn saved_roots_contract() -> Json {
    let mut contract = production_contract("saved-root listing API");
    contract["basis"] = json!("Marrow typed saved-root data-view facts");
    contract["dataAccess"] = json!("gated");
    contract["pathSurface"] = json!({
        "rootOnly": true,
        "childPaths": "absent",
        "editorAuthoredSavedPath": "blocked",
    });
    contract
}

pub fn data_children_contract() -> Json {
    let mut contract = production_contract("bounded typed data children API");
    contract["basis"] = json!("Marrow typed data children tooling");
    contract["dataAccess"] = json!("gated");
    contract["boundedness"] = json!({
        "page": "limit-clamped",
        "unboundedWalk": false,
    });
    contract["pathSurface"] = json!({
        "segments": "typed-saved-data-path",
        "cursor": "typed",
        "memberExpansion": "Marrow-owned",
        "savedPathString": "absent",
    });
    contract
}

pub fn data_read_contract() -> Json {
    let mut contract = production_contract("bounded typed data read API");
    contract["basis"] = json!("Marrow typed data read tooling");
    contract["dataAccess"] = json!("gated");
    contract["boundedness"] = json!({
        "valuePreview": "limit-clamped",
    });
    contract["presenceDetection"] = json!("Marrow-owned bounded read presence");
    contract["pathSurface"] = json!({
        "segments": "typed-saved-data-path",
        "savedPathString": "absent",
    });
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

/// Mark a result as a tool-internal failure — a transport, IO, project-load, or
/// resource-limit fault, as opposed to a Marrow diagnostic or a domain operation
/// error the tool answered with. The transport reads this bit to set the MCP
/// `isError` flag, so an agent keying on `isError` sees tool faults but not a
/// program that legitimately failed to check.
fn tool_error(mut result: Json) -> Json {
    if let Some(object) = result.as_object_mut() {
        object.insert("tool_error".to_string(), json!(true));
    }
    result
}

/// Proof that the operator opted this session into reading a project's real stored
/// data. A read-only data tool takes one, so the read path is entered only with an
/// explicit read grant — never a shared bit that also authorizes writes.
#[derive(Clone, Copy)]
pub struct ReadAccess(());

/// Proof that the operator opted this session into writing a project's real stored
/// data. Distinct from [`ReadAccess`]: [`surface_write`] takes a `WriteAccess` and
/// nothing coerces a read grant into one, so a read grant cannot reach the writable
/// executor. The operator's launch policy is the sole minter of either grant.
#[derive(Clone, Copy)]
pub struct WriteAccess(());

impl ReadAccess {
    /// The read grant the transport hands a data read when the operator opted in.
    pub fn granted() -> Self {
        Self(())
    }
}

impl WriteAccess {
    /// The write grant the transport hands [`surface_write`] when the operator
    /// opted into writes specifically, separately from reads.
    pub fn granted() -> Self {
        Self(())
    }
}

/// The append-only file, at the project root, that records every granted surface
/// write. A data-enabled server that mutates the real store leaves a durable trail
/// beside it, so an operator can reconstruct exactly what an agent changed.
const WRITE_AUDIT_FILE: &str = "marrow-mcp-write-audit.jsonl";

/// One entry in the write-audit trail: when the write ran, which operation ran
/// against which entry file, the record the operation targeted, whether it
/// succeeded, and the store's identity immediately before and after — so a reader
/// can tell what an agent aimed at and whether the store advanced. The target is
/// the operation's identity (its store and keys), never the field values written,
/// so the trail names what was touched without copying stored data into itself.
#[derive(Clone, serde::Serialize)]
struct WriteAuditRecord {
    /// Milliseconds since the Unix epoch when the write was recorded.
    timestamp_ms: u64,
    operation_tag: String,
    file: PathBuf,
    request_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<SurfaceIdentityJson>,
    outcome: WriteOutcome,
    store_before: Option<AuditStoreStamp>,
    store_after: Option<AuditStoreStamp>,
}

/// Whether the audited operation reported success or an error. The store may have
/// been mutated before an error surfaced, so the outcome records how the operation
/// answered, not whether the store is unchanged.
#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum WriteOutcome {
    Ok,
    Error,
}

/// Milliseconds since the Unix epoch, saturating a pre-epoch clock to zero and an
/// implausibly far-future clock to the `u64` ceiling. Dependency-free so the audit
/// trail carries a timestamp without pulling in a date library.
fn unix_millis_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// The identity a surface operation targets — its store and keys — read off the
/// typed request body. Only the record-addressing operations carry an identity;
/// singleton and action operations target no specific keyed record, so they audit
/// with no target rather than a fabricated one.
fn audit_target(body: &SurfaceOperationRequestBodyJson) -> Option<SurfaceIdentityJson> {
    match body {
        SurfaceOperationRequestBodyJson::PointRead { request } => Some(request.identity.clone()),
        SurfaceOperationRequestBodyJson::PointUpdate { request } => Some(request.identity.clone()),
        SurfaceOperationRequestBodyJson::PointCreate { request } => Some(request.identity.clone()),
        SurfaceOperationRequestBodyJson::PointDelete { request } => Some(request.identity.clone()),
        _ => None,
    }
}

/// The stable snake_case name of a surface request body's kind, taken from the
/// typed variant rather than re-read off raw JSON, so the checker and the audit
/// agree on one spelling per operation.
fn request_kind_name(body: &SurfaceOperationRequestBodyJson) -> &'static str {
    match body {
        SurfaceOperationRequestBodyJson::SingletonRead { .. } => "singleton_read",
        SurfaceOperationRequestBodyJson::PointRead { .. } => "point_read",
        SurfaceOperationRequestBodyJson::Page { .. } => "page",
        SurfaceOperationRequestBodyJson::UniqueLookup { .. } => "unique_lookup",
        SurfaceOperationRequestBodyJson::SingletonUpdate { .. } => "singleton_update",
        SurfaceOperationRequestBodyJson::PointUpdate { .. } => "point_update",
        SurfaceOperationRequestBodyJson::SingletonCreate { .. } => "singleton_create",
        SurfaceOperationRequestBodyJson::PointCreate { .. } => "point_create",
        SurfaceOperationRequestBodyJson::SingletonDelete { .. } => "singleton_delete",
        SurfaceOperationRequestBodyJson::PointDelete { .. } => "point_delete",
        SurfaceOperationRequestBodyJson::Action { .. } => "action",
        SurfaceOperationRequestBodyJson::ComputedRead { .. } => "computed_read",
    }
}

/// The store identity captured in an audit record: the durable store id and its
/// commit counter, enough to prove a write moved the store forward.
#[derive(Clone, serde::Serialize)]
struct AuditStoreStamp {
    store_uid: String,
    commit_id: u64,
}

impl From<&StoreStamp> for AuditStoreStamp {
    fn from(stamp: &StoreStamp) -> Self {
        Self {
            store_uid: stamp.store_uid.clone(),
            commit_id: stamp.commit_id,
        }
    }
}

/// Append one JSON line to the project's write-audit trail. Best-effort: the store
/// mutation is already durable by the time we record it, so a failed append is
/// surfaced to the caller rather than pretended away, but it cannot roll the write
/// back.
fn append_write_audit(root: &Path, record: &WriteAuditRecord) -> Result<(), String> {
    use std::io::Write as _;
    let line = serde_json::to_string(record).map_err(|error| error.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(WRITE_AUDIT_FILE))
        .map_err(|error| error.to_string())?;
    writeln!(file, "{line}").map_err(|error| error.to_string())
}

thread_local! {
    /// One [`Workspace`] kept for the life of the transport process, so back-to-back
    /// tool calls on an unchanged project reuse its checked analysis instead of a
    /// cold check every time. The MCP transport is single-threaded, so a
    /// thread-local holds the whole cache without a lock; a `mw_run` on its own
    /// watchdog thread simply gets its own fresh cache.
    static PROJECT_CACHE: RefCell<CachedProject> = RefCell::new(CachedProject::default());
}

/// The cached project analysis plus fingerprints of the exact source set and
/// committed catalog it was checked against, so a change to either between calls
/// re-checks.
#[derive(Default)]
struct CachedProject {
    workspace: Workspace,
    source_fingerprint: Option<u64>,
    catalog_fingerprint: Option<u64>,
}

impl CachedProject {
    /// Whether the cached analysis still answers for `file`: the file must live
    /// under the cached project root, the analyzed source set (with any overlay)
    /// must hash identically, and the committed catalog must not have advanced.
    ///
    /// The MCP transport has no file watcher and rebuilds its document set on every
    /// call, so it cannot lean on the LSP's document-generation freshness token; it
    /// re-derives the source fingerprint from disk each call. That walk is far
    /// cheaper than the full recompute it guards.
    fn is_fresh_for(&self, file: &Path, overlay: Option<(&Path, &str)>) -> bool {
        let Some(project) = self.workspace.project() else {
            return false;
        };
        file.starts_with(&project.root)
            && source_fingerprint(project, overlay) == self.source_fingerprint
            && catalog_fingerprint(project) == self.catalog_fingerprint
    }
}

/// A fingerprint of every source and test module the project analyzes — the overlay
/// text for the edited file, disk content for the rest. It changes when a file's
/// content changes and when a file is added to or removed from the project, so it
/// busts the cache on any source change a recompute would observe.
fn source_fingerprint(project: &Project, overlay: Option<(&Path, &str)>) -> Option<u64> {
    let mut paths: Vec<PathBuf> = discover_modules(&project.root, &project.config)
        .ok()?
        .into_iter()
        .chain(discover_test_modules(&project.root, &project.config).ok()?)
        .map(|module| module.path)
        .collect();
    paths.sort();
    paths.dedup();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for path in &paths {
        std::hash::Hash::hash(path, &mut hasher);
        let content = match overlay {
            Some((overlay_path, source)) if overlay_path == path => source.to_string(),
            _ => std::fs::read_to_string(path).unwrap_or_default(),
        };
        std::hash::Hash::hash(&content, &mut hasher);
    }
    Some(std::hash::Hasher::finish(&hasher))
}

/// A cheap fingerprint of a project's committed catalog lock, or `None` when it
/// has none (a memory-backed project keeps no committed catalog). It changes when
/// the store commits, so a catalog advance busts the source-keyed cache.
fn catalog_fingerprint(project: &Project) -> Option<u64> {
    let bytes = std::fs::read(project.root.join(COMMITTED_LOCK_FILE)).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash_slice(&bytes, &mut hasher);
    Some(std::hash::Hasher::finish(&hasher))
}

thread_local! {
    /// The directory tree this thread's project-loading tools are confined to.
    /// `None` — the default the LSP brain and confinement-agnostic tests leave in
    /// place — confines nothing; the MCP transport sets it once at launch from its
    /// immutable policy, and a sandboxed `mw_run` re-applies it on its watchdog
    /// thread. When set, a tool refuses any file whose canonical location, or whose
    /// resolved project root, escapes the tree, before it opens a store or analyzes.
    static ALLOWED_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Confine every project-loading tool on this thread to `root` for the rest of the
/// session. The MCP transport calls this once at launch; because the confinement
/// lives in a thread-local, `mw_run`'s watchdog thread re-applies it so a run
/// cannot escape a boundary the transport honors.
pub fn set_allowed_root(root: PathBuf) {
    ALLOWED_ROOT.with(|cell| *cell.borrow_mut() = Some(root));
}

/// The directory the current thread's tools are confined to, if any.
fn current_allowed_root() -> Option<PathBuf> {
    ALLOWED_ROOT.with(|cell| cell.borrow().clone())
}

/// Why a project could not be loaded for a tool.
#[derive(Debug)]
enum ProjectAccessError {
    /// The file, or the project it resolves to, lies outside the allowed root.
    OutOfRoot,
    /// The project could not be resolved or checked; carries the render message.
    Load(String),
}

impl ProjectAccessError {
    /// Render this failure into a tool result. An out-of-root refusal is a uniform
    /// typed DTO across every tool; any other failure is rendered by `load` into the
    /// tool's own error envelope.
    fn render(self, load: impl FnOnce(String) -> Json) -> Json {
        match self {
            ProjectAccessError::OutOfRoot => out_of_root_refusal(),
            ProjectAccessError::Load(message) => load(message),
        }
    }
}

/// The refusal a project-loading tool returns when confinement rejects its file.
/// Identical whether the outside path exists, so it reveals nothing about the
/// filesystem beyond the boundary; not marked a tool fault, since a confined server
/// refusing an out-of-root path is behaving exactly as configured.
fn out_of_root_refusal() -> Json {
    json!({
        "available": false,
        "refusal": "path.out_of_root",
        "message": "the requested file resolves outside the marrow-mcp allowed root",
    })
}

/// Reject `file` when the session is confined and the file — or the project it
/// resolves to — escapes the allowed root. The input path is checked first, so an
/// outside path is refused before any project walk touches it; then the resolved
/// project root is checked, so a file inside the root whose `marrow.json` sits
/// outside it is refused too. Both comparisons are component-wise over canonical
/// paths, never a string prefix.
fn confinement_check(file: &Path) -> Result<(), ProjectAccessError> {
    let Some(root) = current_allowed_root() else {
        return Ok(());
    };
    let Some(root) = canonical_existing_prefix(&root) else {
        // A confined session whose root cannot be resolved fails closed: admitting
        // everything would silently drop the boundary.
        return Err(ProjectAccessError::OutOfRoot);
    };
    if !path_within(&root, file) {
        return Err(ProjectAccessError::OutOfRoot);
    }
    if let Some(project_root) = crate::workspace::project_root_of(file)
        && !path_within(&root, &project_root)
    {
        return Err(ProjectAccessError::OutOfRoot);
    }
    Ok(())
}

/// Whether `target` lies within `root` once both are reduced to real, absolute
/// locations. `target` may not exist yet, so its existing prefix is canonicalized
/// (resolving symlinks and `..`) and the trailing not-yet-created components are
/// folded lexically — a non-existent path cannot smuggle a parent escape past the
/// check, and a symlink cannot point the existing prefix outside undetected.
fn path_within(root: &Path, target: &Path) -> bool {
    match canonical_existing_prefix(target) {
        Some(resolved) => is_within(root, &resolved),
        None => false,
    }
}

/// Canonicalize the longest existing prefix of `path`, then re-attach the trailing
/// components that do not exist yet, folding `.`/`..` lexically. Returns `None` only
/// when no prefix resolves, which the caller treats as outside the boundary.
fn canonical_existing_prefix(path: &Path) -> Option<PathBuf> {
    let mut trailing: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = path.to_path_buf();
    loop {
        if let Ok(mut resolved) = probe.canonicalize() {
            for name in trailing.iter().rev() {
                if name == ".." {
                    resolved.pop();
                } else if name != "." {
                    resolved.push(name);
                }
            }
            return Some(resolved);
        }
        let name = probe.file_name()?.to_os_string();
        trailing.push(name);
        if !probe.pop() {
            return None;
        }
    }
}

/// Whether `target`'s path components begin with all of `root`'s, so `target` is
/// `root` itself or nested beneath it.
fn is_within(root: &Path, target: &Path) -> bool {
    let mut root_components = root.components();
    let mut target_components = target.components();
    loop {
        match root_components.next() {
            None => return true,
            Some(root_component) => match target_components.next() {
                Some(target_component) if target_component == root_component => continue,
                _ => return false,
            },
        }
    }
}

/// Load the project `file` belongs to (walking up to its `marrow.json`), check it
/// if the cache is cold or stale, and run `f` against the checked workspace and
/// the file's absolute path. `source`, when given, overlays the file's on-disk
/// text so analysis reflects an agent's unsaved edit; a distinct overlay counts as
/// a source change and re-checks. Confinement is enforced before any project walk,
/// so an out-of-root file never reaches analysis; a file outside any project, or
/// one that cannot be checked, yields a load error for the tool's envelope.
fn with_loaded_project<R>(
    file: &Path,
    source: Option<&str>,
    f: impl FnOnce(&Workspace, &Path) -> R,
) -> Result<R, ProjectAccessError> {
    confinement_check(file)?;
    let overlay = source.map(|source| (file, source));
    PROJECT_CACHE.with(|cache| {
        let cache = &mut *cache.borrow_mut();
        if !cache.is_fresh_for(file, overlay) {
            record_recompute();
            let mut documents = Documents::new();
            if let Some(source) = source {
                // Overlay the buffer the same way the LSP overlays an open editor
                // buffer, so a project file with an unsaved edit checks against it.
                let url = path_to_url(file).ok_or_else(|| {
                    ProjectAccessError::Load("the file path is not absolute".to_string())
                })?;
                documents.open(url, source.to_string());
            }
            let sources = documents.analysis_snapshot();
            cache
                .workspace
                .recompute_from_snapshot(file, &sources)
                .map_err(|error| ProjectAccessError::Load(error.to_string()))?;
            let (source_fp, catalog_fp) = match cache.workspace.project() {
                Some(project) => (
                    source_fingerprint(project, overlay),
                    catalog_fingerprint(project),
                ),
                None => (None, None),
            };
            cache.source_fingerprint = source_fp;
            cache.catalog_fingerprint = catalog_fp;
        }
        Ok(f(&cache.workspace, file))
    })
}

/// Count a cold-or-stale re-check, so a cache test can assert that an unchanged
/// project is not re-checked. A no-op outside tests.
fn record_recompute() {
    #[cfg(test)]
    RECOMPUTE_COUNT.with(|count| count.set(count.get() + 1));
}

#[cfg(test)]
thread_local! {
    static RECOMPUTE_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn recompute_count() -> u64 {
    RECOMPUTE_COUNT.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_project_cache() {
    PROJECT_CACHE.with(|cache| *cache.borrow_mut() = CachedProject::default());
    RECOMPUTE_COUNT.with(|count| count.set(0));
}

/// Lift confinement on this thread, so a confinement test cannot leak its allowed
/// root onto a thread the harness later reuses for another test.
#[cfg(test)]
pub(crate) fn clear_allowed_root() {
    ALLOWED_ROOT.with(|cell| *cell.borrow_mut() = None);
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
    with_profile_version(check_result(file, source), CHECK_PROFILE_VERSION)
}

fn check_result(file: Option<&Path>, source: Option<&str>) -> Json {
    match file {
        Some(file) => match with_loaded_project(file, source, |workspace, file| {
            let snapshot = workspace.latest().expect("recompute stored a snapshot");
            // Only the requested file's diagnostics: an agent asked about one
            // file, and the rest of the project's problems are noise to it.
            let diagnostics: Vec<Json> = snapshot
                .report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.file.as_path() == file)
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
        }) {
            Ok(result) => result,
            Err(err) => {
                err.render(|error| tool_error(json!({ "diagnostics": [], "error": error })))
            }
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
    with_profile_version(
        type_at_result(file, line, character),
        TYPE_AT_PROFILE_VERSION,
    )
}

fn type_at_result(file: &Path, line: u32, character: u32) -> Json {
    match with_loaded_project(file, None, |workspace, file| {
        let snapshot = workspace.latest().expect("recompute stored a snapshot");
        let Some(analyzed) = snapshot.files.iter().find(|f| f.path.as_path() == file) else {
            return json!({ "type": Json::Null });
        };
        // The parse was built from the file's on-disk text; index that same text so
        // the position maps to the offset the parse covers.
        let source = read_to_string(file);
        let index = crate::positions::LineIndex::new(source);
        let offset = index.offset(Position { line, character });
        let ty = type_at(&snapshot.program, file, &analyzed.parsed, offset);
        json!({ "type": ty.map(|ty| render_type(&ty)) })
    }) {
        Ok(result) => result,
        Err(err) => err.render(|error| tool_error(json!({ "type": Json::Null, "error": error }))),
    }
}

/// `mw_complete`: the Marrow `source.completion.v1` fact at a position in
/// `file`. The result contains the fact profile version plus items shaped as
/// `{ label, kind, detail?, docs }`.
pub fn complete(file: &Path, line: u32, character: u32) -> Json {
    let contract = completion_contract();
    let result = with_loaded_project(file, None, |workspace, file| {
        let snapshot = workspace.latest().expect("recompute stored a snapshot");
        let Some(analyzed) = snapshot.files.iter().find(|f| f.path.as_path() == file) else {
            return empty_completion_result(None);
        };
        // The classifier reads the cached lex's spans against this text, and the
        // parse came from the same on-disk text, so read and index that text.
        let source = read_to_string(file);
        let index = crate::positions::LineIndex::new(&source);
        let offset = index.offset(Position { line, character });
        let lexed = marrow_syntax::lex_source(&source);
        let fact = source_completion_fact(
            &snapshot.program,
            file,
            &source,
            &analyzed.parsed,
            &lexed,
            offset,
        );
        let items: Vec<Json> = fact
            .items
            .into_iter()
            .map(|item| {
                json!({
                    "label": item.label,
                    "kind": completion_kind_name(item.kind),
                    "detail": item.detail,
                    "docs": item.docs,
                })
            })
            .collect();
        json!({ "profile_version": fact.profile_version, "items": items })
    });
    match result {
        Ok(result) => with_contract(result, contract),
        Err(err) => with_contract(
            err.render(|error| tool_error(empty_completion_result(Some(error)))),
            contract,
        ),
    }
}

fn empty_completion_result(error: Option<String>) -> Json {
    let mut result = json!({
        "profile_version": SOURCE_COMPLETION_PROFILE_VERSION,
        "items": [],
    });
    if let Some(error) = error {
        result["error"] = json!(error);
    }
    result
}

/// `mw_namespace_complete`: Marrow's source namespace completion fact for a
/// concrete module or enum qualifier. This is a production fact surface, not the
/// broad editor completion helper: no cursor recovery, source overlay, stdlib
/// fallback, or `CompletionItem` presentation is accepted here.
pub fn namespace_complete(file: &Path, qualifier: &[String]) -> Json {
    let contract = namespace_completion_contract();
    let result = with_loaded_project(file, None, |workspace, file| {
        let snapshot = workspace.latest().expect("recompute stored a snapshot");
        let Some(analyzed) = snapshot.files.iter().find(|f| f.path.as_path() == file) else {
            return empty_namespace_completion_result(None);
        };
        match source_namespace_completion_file_fact(
            &snapshot.program,
            file,
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
        }
    });
    match result {
        Ok(result) => with_contract(result, contract),
        Err(err) => with_contract(
            err.render(|error| tool_error(empty_namespace_completion_result(Some(error)))),
            contract,
        ),
    }
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
    let result = with_loaded_project(file, None, |workspace, _| {
        let Some(program) = workspace.program() else {
            return json!({
                "profile_version": RESOURCE_SCHEMA_PROFILE_VERSION,
                "resources": [],
                "diagnostics": [{
                    "code": "resource.schema.missing",
                    "message": "no checked program is available for resource schema lookup"
                }],
            });
        };
        serde_json::to_value(marrow_resource_schema_for_name(program, name))
            .expect("Marrow resource schema DTO serializes")
    });
    match result {
        Ok(result) => with_contract(result, contract),
        Err(err) => with_contract(
            err.render(|error| {
                tool_error(json!({
                    "profile_version": RESOURCE_SCHEMA_PROFILE_VERSION,
                    "resources": [],
                    "diagnostics": [],
                    "error": error,
                }))
            }),
            contract,
        ),
    }
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
    let result = with_loaded_project(file, None, |workspace, _| {
        let Some(program) = workspace.program() else {
            return empty_surface_route_manifest("no checked program");
        };
        let abi = SurfaceAbiJson::from_program(program);
        let mut manifest = SurfaceRouteManifestJson::from_abi(&abi);
        if scope == SurfaceRouteScope::ReadOnly {
            manifest
                .routes
                .retain(|route| SurfaceOperationKind::from(&route.request).is_read());
        }
        serde_json::to_value(manifest).expect("surface route manifest DTO serializes")
    });
    match result {
        Ok(result) => with_contract(result, contract),
        Err(err) => with_contract(
            err.render(|error| tool_error(empty_surface_route_manifest(error))),
            contract,
        ),
    }
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
pub fn surface_read(
    file: &Path,
    operation: Json,
    access: Option<ReadAccess>,
) -> Result<Json, String> {
    let contract = surface_read_contract();
    if access.is_none() {
        return Ok(read_disabled(contract));
    }
    let request: SurfaceOperationRequestJson = serde_json::from_value(operation)
        .map_err(|error| format!("invalid mw_surface_read operation: {error}"))?;
    let result = with_loaded_project(file, None, |workspace, _| {
        let Some(project) = workspace.project() else {
            return json!({ "available": false, "error": "no project resolved for the file" });
        };
        let session = match ProjectSurfaceReadSession::open(&project.root) {
            Ok(session) => session,
            Err(error) => return json!({ "available": false, "error": error.message() }),
        };
        let store_stamp = match session.store_stamp() {
            Ok(stamp) => store_stamp_json(stamp),
            Err(error) => return json!({ "available": false, "error": error.message() }),
        };
        match execute_project_surface_operation_read_only(&session, &request) {
            Ok(response) => {
                json!({ "available": true, "store_stamp": store_stamp, "response": response })
            }
            Err(error) => json!({ "available": true, "store_stamp": store_stamp, "error": error }),
        }
    });
    Ok(match result {
        Ok(result) => with_contract(result, contract),
        Err(err) => with_contract(
            err.render(|error| tool_error(json!({ "available": false, "error": error }))),
            contract,
        ),
    })
}

/// `mw_surface_write`: execute one canonical `surface.operation.v1` request
/// through Marrow's writable project surface executor. The MCP data-access gate
/// is checked before project or store opening; operation bodies are admitted only
/// by the Marrow JSON DTO and executor.
pub fn surface_write(
    file: &Path,
    operation: Json,
    access: Option<WriteAccess>,
) -> Result<Json, String> {
    let contract = surface_write_contract();
    if access.is_none() {
        return Ok(write_disabled(contract));
    }
    // Decode the canonical DTO first, then read the operation tag, kind, and target
    // off the typed request. One deserialize is the single owner of what the write
    // is, so the audit never re-parses the raw envelope for a second opinion.
    let request: SurfaceOperationRequestJson = serde_json::from_value(operation)
        .map_err(|error| format!("invalid mw_surface_write operation: {error}"))?;
    let operation_tag = request.operation_tag.clone();
    let request_kind = request_kind_name(&request.request);
    let target = audit_target(&request.request);
    let result = with_loaded_project(file, None, |workspace, resolved_file| {
        let Some(project) = workspace.project() else {
            return json!({ "available": false, "error": "no project resolved for the file" });
        };
        let session = match ProjectSurfaceSession::open(&project.root) {
            Ok(session) => session,
            Err(error) => return json!({ "available": false, "error": error.message() }),
        };
        let store_before = session.store_stamp().ok();
        let operation_result = execute_project_surface_operation(&session, &request);
        // A durably-committed write must always be audited, so the after-stamp is
        // best-effort: if the store cannot be re-read here the mutation may still
        // have landed, and the record captures that with a missing after-stamp
        // rather than dropping the entry entirely.
        let store_after = session.store_stamp().ok();
        let outcome = match &operation_result {
            Ok(_) => WriteOutcome::Ok,
            Err(_) => WriteOutcome::Error,
        };
        let record = WriteAuditRecord {
            timestamp_ms: unix_millis_now(),
            operation_tag: operation_tag.clone(),
            file: resolved_file.to_path_buf(),
            request_kind: request_kind.to_string(),
            target: target.clone(),
            outcome,
            store_before: store_before.as_ref().map(AuditStoreStamp::from),
            store_after: store_after.as_ref().map(AuditStoreStamp::from),
        };
        let audit_error = append_write_audit(&project.root, &record).err();
        let audit = serde_json::to_value(&record).expect("write-audit record serializes");
        let store_stamp = store_after
            .as_ref()
            .map(|stamp| store_stamp_json(stamp.clone()));
        let mut value = match operation_result {
            Ok(response) => {
                json!({ "available": true, "store_stamp": store_stamp, "response": response })
            }
            Err(error) => json!({ "available": true, "store_stamp": store_stamp, "error": error }),
        };
        value["audit"] = audit;
        if let Some(error) = audit_error {
            value["audit_error"] = json!(error);
        }
        value
    });
    Ok(match result {
        Ok(result) => with_contract(result, contract),
        Err(err) => with_contract(
            err.render(|error| tool_error(json!({ "available": false, "error": error }))),
            contract,
        ),
    })
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
pub fn saved_roots(file: &Path, access: Option<ReadAccess>) -> Json {
    let contract = saved_roots_contract();
    if access.is_none() {
        return read_disabled(contract);
    }
    let result = with_loaded_project(file, None, |workspace, _| {
        let session = match data_session(workspace) {
            Ok(session) => session,
            Err(error) => return json!({ "available": false, "roots": [], "error": error }),
        };
        let Some(session) = session else {
            return json!({ "available": false, "roots": [] });
        };
        serde_json::to_value(crate::data_explorer::saved_roots(Some(&session)))
            .unwrap_or_else(|_| json!({ "available": false, "roots": [] }))
    });
    match result {
        Ok(result) => with_contract(result, contract),
        Err(err) => with_contract(
            err.render(|error| {
                tool_error(json!({ "available": false, "roots": [], "error": error }))
            }),
            contract,
        ),
    }
}

/// `mw_data_children`: a bounded page of typed child segments under a saved-data
/// path. Gated like [`saved_roots`]: disabled data access refuses before project
/// loading, while enabled access checks the project and opens the native store
/// read-only for one shared Marrow session read.
pub fn data_children(
    file: &Path,
    request: DataChildrenRequest,
    access: Option<ReadAccess>,
) -> Json {
    let contract = data_children_contract();
    if access.is_none() {
        return read_disabled(contract);
    }
    let unavailable = json!({
        "available": false,
        "children": [],
        "truncated": false,
        "cursor": Json::Null,
        "store_snapshot": Json::Null,
    });
    let result = with_loaded_project(file, None, |workspace, _| {
        let session = match data_session(workspace) {
            Ok(session) => session,
            Err(error) => {
                let mut value = unavailable.clone();
                value["error"] = json!(error);
                return value;
            }
        };
        let Some(session) = session else {
            return unavailable.clone();
        };
        match session_data_child_views_page(&session, request) {
            Ok(result) => {
                let mut result =
                    serde_json::to_value(result).expect("data children DTO must serialize");
                result["available"] = json!(true);
                result
            }
            Err(error) => json!({
                "available": true,
                "children": [],
                "truncated": false,
                "cursor": Json::Null,
                "store_snapshot": Json::Null,
                "error": error,
            }),
        }
    });
    match result {
        Ok(result) => with_contract(result, contract),
        Err(err) => with_contract(
            err.render(|error| {
                let mut value = unavailable;
                value["error"] = json!(error);
                tool_error(value)
            }),
            contract,
        ),
    }
}

/// `mw_data_read`: a bounded rendered preview for one typed saved-data path.
/// Gated like [`data_children`]: disabled data access refuses before project
/// loading, while enabled access checks the project and opens the native store
/// read-only for one shared Marrow session read.
pub fn data_read(file: &Path, request: DataReadRequest, access: Option<ReadAccess>) -> Json {
    let contract = data_read_contract();
    if access.is_none() {
        return read_disabled(contract);
    }
    let unavailable = json!({
        "available": false,
        "presence": "absent",
        "value": Json::Null,
        "value_truncated": false,
        "store_snapshot": Json::Null,
    });
    let result = with_loaded_project(file, None, |workspace, _| {
        let session = match data_session(workspace) {
            Ok(session) => session,
            Err(error) => {
                let mut value = unavailable.clone();
                value["error"] = json!(error);
                return value;
            }
        };
        let Some(session) = session else {
            return unavailable.clone();
        };
        match session_data_read(&session, request) {
            Ok(result) => {
                let mut result =
                    serde_json::to_value(result).expect("data read DTO must serialize");
                result["available"] = json!(true);
                result
            }
            Err(error) => json!({
                "available": true,
                "presence": "absent",
                "value": Json::Null,
                "value_truncated": false,
                "store_snapshot": Json::Null,
                "error": error,
            }),
        }
    });
    match result {
        Ok(result) => with_contract(result, contract),
        Err(err) => with_contract(
            err.render(|error| {
                let mut value = unavailable;
                value["error"] = json!(error);
                tool_error(value)
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

/// The refusal a read data tool returns when read access is not enabled: a clear,
/// machine-readable envelope that carries no stored data. The transport sets the
/// opt-in (an env var / launch flag); the boundary itself lives here so a missing
/// opt-in can never leak a byte regardless of transport.
fn read_disabled(contract: Json) -> Json {
    with_contract(
        json!({
            "available": false,
            "dataAccess": "disabled",
            "message": "data access not enabled; relaunch marrow-mcp with MARROW_MCP_ALLOW_DATA=1 (or --allow-data) to read stored data",
        }),
        contract,
    )
}

/// The refusal [`surface_write`] returns when the write opt-in is not set. Write
/// access is a separate grant from read: a session may read stored data yet still
/// refuse to mutate it, so the write refusal names its own opt-in and never opens
/// the writable executor.
fn write_disabled(contract: Json) -> Json {
    with_contract(
        json!({
            "available": false,
            "dataAccess": "disabled",
            "writeOptIn": "disabled",
            "message": "write access not enabled; relaunch marrow-mcp with MARROW_MCP_ALLOW_WRITE=1 (or --allow-write) to mutate stored data",
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

/// The wall-clock budget a single `mw_run` is allowed before the watchdog gives
/// up on it. Marrow imposes no per-loop step cap — terminating a loop is the
/// program author's contract — so a `while true` entry runs forever; without a
/// deadline it wedges the single-threaded transport permanently. The operator
/// overrides this at launch. Cooperatively unwinding a wedged run in place is
/// upstream work; until it exists, an over-budget run is detached, not stopped.
pub const DEFAULT_RUN_BUDGET: Duration = Duration::from_secs(30);

/// The ceiling on a run's captured `print` output. A loop that prints can emit
/// unbounded text, so the sink stops appending past this many bytes and the
/// envelope carries `output_truncated: true` to tell the agent output was cut.
const OUTPUT_CAP: usize = 64 * 1024;

/// `mw_run`: execute Marrow to confirm behavior — always sandboxed and always
/// bounded by `budget`.
///
/// **Run mode** checks the project, then evaluates `entry` through Marrow's
/// fresh-memory project session under a locked-down [`Host`] (a fixed clock and a
/// captured log only), returning Marrow's typed run envelope. **Test mode** runs
/// each discovered test over its own fresh store; `entry` is ignored and `args`
/// are rejected. The real store is never opened in either mode.
///
/// The evaluation runs on a watchdog thread so a run that loops past the budget
/// is detached and reported as `budget_exceeded`, leaving the transport free to
/// answer the next call. The detached thread holds only a fresh in-memory store,
/// so abandoning it leaks no durable lock — but a runaway loop keeps burning one
/// CPU core until process exit, since there is no cooperative interrupt to unwind
/// it. Bounding that cleanly needs a run-session step/time budget the runtime does
/// not yet expose.
pub fn run(
    file: &Path,
    entry: Option<&str>,
    args: &[Json],
    mode: RunMode,
    budget: Duration,
) -> Json {
    with_profile_version(
        with_contract(
            run_within_budget(file, entry, args, mode, budget),
            run_contract(),
        ),
        RUN_PROFILE_VERSION,
    )
}

/// Run the evaluation on a detached watchdog thread and wait up to `budget`. The
/// whole run — project check, session open, and invoke — happens on the thread,
/// so a program that wedges anywhere in that path cannot block the transport. A
/// thread that panics closes its channel without sending, which surfaces as a
/// typed tool fault rather than a hang.
fn run_within_budget(
    file: &Path,
    entry: Option<&str>,
    args: &[Json],
    mode: RunMode,
    budget: Duration,
) -> Json {
    let file = file.to_path_buf();
    let entry = entry.map(str::to_string);
    let args = args.to_vec();
    // A freshly spawned thread starts unconfined, so carry the caller thread's
    // allowed root onto the watchdog thread; a sandboxed run must honor the same
    // boundary the transport set for every other tool.
    let allowed_root = current_allowed_root();
    let (sender, receiver) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("marrow-mcp-run".to_string())
        .spawn(move || {
            if let Some(root) = allowed_root {
                set_allowed_root(root);
            }
            let _ = sender.send(run_result(&file, entry.as_deref(), &args, mode));
        })
        .expect("spawning the run watchdog thread");
    match receiver.recv_timeout(budget) {
        Ok(result) => result,
        // The run outlived its budget. Abandon the thread (it keeps running on its
        // own fresh in-memory store until the process exits) so the transport is
        // freed, and report the deadline as a typed fault.
        Err(RecvTimeoutError::Timeout) => {
            drop(handle);
            run_budget_exceeded(budget)
        }
        // The thread dropped its sender without a result: it panicked. That is a
        // tool-internal fault, not a Marrow diagnostic, so it is surfaced as one.
        Err(RecvTimeoutError::Disconnected) => run_thread_faulted(),
    }
}

/// The typed envelope for a run that exceeded its wall-clock budget: a stable
/// `budget_exceeded` marker an agent can key on, plus a diagnostic that renders
/// in the one-line summary. `tool_error` marks it a tool-internal fault so the
/// transport can set `isError`.
fn run_budget_exceeded(budget: Duration) -> Json {
    tool_error(json!({
        "diagnostics": [{
            "code": "mcp.run.budget_exceeded",
            "message": format!(
                "run exceeded the {}s wall-clock budget and was detached",
                budget.as_secs()
            ),
        }],
        "output": "",
        "budget_exceeded": true,
        "budget_seconds": budget.as_secs(),
    }))
}

/// The typed envelope for a run whose watchdog thread panicked: a tool-internal
/// fault, distinct from a program that legitimately failed to check.
fn run_thread_faulted() -> Json {
    tool_error(json!({
        "diagnostics": [{
            "code": "mcp.run.thread_faulted",
            "message": "the run thread ended without a result",
        }],
        "output": "",
    }))
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
        Err(err) => {
            return err.render(|error| {
                tool_error(json!({ "diagnostics": [{ "message": error }], "output": "" }))
            });
        }
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

/// Load `file`'s project far enough to find the root Marrow will run. Confinement
/// runs inside `with_loaded_project`, so an out-of-root run is refused here before
/// any session opens.
fn load_project_for_run(file: &Path) -> Result<PathBuf, ProjectAccessError> {
    with_loaded_project(file, None, |workspace, _| {
        workspace.project().map(|project| project.root.clone())
    })?
    .ok_or_else(|| ProjectAccessError::Load("no project resolved for the file".to_string()))
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
    let mut output = CappedOutput::new();
    let invoke = session.invoke(SessionEntry::protocol(invocation, &host, &mut output));
    let (output, truncated) = output.into_parts();
    let result = match invoke {
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
    with_run_facts(flag_output_truncated(result, truncated), run_facts)
}

/// A [`marrow_run::RunOutputSink`] that retains at most [`OUTPUT_CAP`] bytes. Once
/// full it drops further writes and records that it truncated, so an unbounded
/// print loop cannot grow captured output without bound while the run executes.
struct CappedOutput {
    text: String,
    truncated: bool,
}

impl CappedOutput {
    fn new() -> Self {
        Self {
            text: String::new(),
            truncated: false,
        }
    }

    fn into_parts(self) -> (String, bool) {
        (self.text, self.truncated)
    }
}

impl marrow_run::RunOutputSink for CappedOutput {
    fn write(&mut self, text: &str) {
        if self.truncated {
            return;
        }
        let remaining = OUTPUT_CAP - self.text.len();
        if text.len() <= remaining {
            self.text.push_str(text);
            return;
        }
        // Keep the largest whole-character prefix that fits, so the retained text
        // is always valid UTF-8 rather than a split code point.
        let mut end = remaining;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&text[..end]);
        self.truncated = true;
    }
}

/// Mark a run envelope whose captured output was cut off at [`OUTPUT_CAP`], so an
/// agent reading the (bounded) output knows more was produced.
fn flag_output_truncated(mut result: Json, truncated: bool) -> Json {
    if let (true, Some(object)) = (truncated, result.as_object_mut()) {
        object.insert("output_truncated".to_string(), json!(true));
    }
    result
}

fn entry_run_facts_json(session: &ProjectSession) -> Option<Json> {
    marrow_json::entry_run_facts_to_json(session)
        .map(|facts| serde_json::to_value(facts).expect("Marrow run facts DTO serializes"))
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

/// The lowercase semantic item kind exposed by `mw_complete`.
fn completion_kind_name(kind: SourceCompletionItemKind) -> &'static str {
    match kind {
        SourceCompletionItemKind::Keyword => "keyword",
        SourceCompletionItemKind::Local => "variable",
        SourceCompletionItemKind::Function => "function",
        SourceCompletionItemKind::Resource => "resource",
        SourceCompletionItemKind::SavedRoot => "saved-root",
        SourceCompletionItemKind::Field => "field",
        SourceCompletionItemKind::Layer => "layer",
        SourceCompletionItemKind::Enum => "enum",
        SourceCompletionItemKind::EnumMember => "enum-member",
        SourceCompletionItemKind::Module => "module",
        SourceCompletionItemKind::StoreIdentity => "store-identity",
    }
}

fn read_to_string(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests;
