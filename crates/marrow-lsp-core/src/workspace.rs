//! Resolve a Marrow project from a file path and check it against open buffers.
//!
//! A project is the directory holding a `marrow.json`. Given a file inside one,
//! the workspace walks up to that file, parses the config, overlays every open
//! buffer under the root onto disk, and runs the project checker — so analysis
//! reflects unsaved edits, matching what `marrow check` would report once saved.

use std::path::{Path, PathBuf};

use lsp_types::Url;
use marrow_catalog::{CatalogLock, CatalogMetadata};
use marrow_check::{
    AnalysisGeneration, ProjectIoError, ProjectSources, analyze_project, build_binding_index,
};
use marrow_project::{ProjectConfig, parse_config, test_module_file};

pub use marrow_check::{AnalysisSnapshot, BindingIndex, CheckedProgram};

use crate::catalog_binding::CatalogBindingCache;
use crate::documents::{DocumentAnalysisSnapshot, Documents};

/// The name of a Marrow project's configuration file; its directory is the root.
const CONFIG_FILE: &str = "marrow.json";
pub const COMMITTED_LOCK_FILE: &str = marrow_project::CATALOG_FILE_NAME;

/// A resolved project: its root directory and parsed configuration.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub config: ProjectConfig,
}

impl Project {
    /// Whether `path` can contribute a file to Marrow project analysis.
    pub fn analyzes_file(&self, path: &Path) -> bool {
        self.source_module_file(path) || test_module_file(&self.root, &self.config, path).is_some()
    }

    fn source_module_file(&self, path: &Path) -> bool {
        if path.extension().and_then(|ext| ext.to_str()) != Some("mw") {
            return false;
        }
        self.config.source_roots.iter().any(|source_root| {
            let root = self.root.join(source_root);
            path.strip_prefix(root).is_ok()
        })
    }
}

/// Why a file could not be resolved to a checkable project.
#[derive(Debug)]
pub enum WorkspaceError {
    /// No `marrow.json` was found walking up from the file.
    NoProject,
    /// A `marrow.json` was found but could not be parsed.
    Config(marrow_project::ConfigError),
    /// The source analysis context could not be read.
    AnalysisContext(ProjectIoError),
    /// The project's source roots could not be walked.
    Discover(marrow_project::DiscoverError),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoProject => write!(f, "no marrow.json found for the file"),
            Self::Config(error) => write!(f, "{error}"),
            Self::AnalysisContext(error) => write!(f, "analysis context: {}", error.message()),
            Self::Discover(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

/// Holds the project resolved for the buffers in play and its latest analysis.
#[derive(Default)]
pub struct Workspace {
    project: Option<Project>,
    latest: Option<AnalysisSnapshot>,
    /// The most recent checked program that had any modules. The checker drops a
    /// module for a file with a parse error, so a mid-edit recompute can yield an
    /// empty program; completion and other schema-driven requests fall back to
    /// this last good program so a stray syntax error does not blank their results.
    last_program: Option<CheckedProgram>,
    /// The binding index for [`Self::latest`], built lazily on the first
    /// navigation request and reused by go-to-definition, find-references, and
    /// rename. A recompute keeps this only when the analysis generation and project
    /// root still match, so unchanged diagnostics publishes do not rebuild it.
    binding_index: Option<BindingIndex>,
    binding_index_key: Option<BindingIndexKey>,
    /// The document generation the latest snapshot was analyzed at. Every open,
    /// edit, and close bumps the generation, so an unchanged generation means the
    /// editor-visible source world is byte-identical to the snapshot's — an O(1)
    /// freshness check that never re-reads the project from disk. A change to a
    /// closed file on disk does not bump the generation; the file watcher's
    /// invalidation refreshes those, the way rust-analyzer and gopls trust their
    /// VFS rather than stat the whole project on every request.
    latest_documents_generation: Option<u64>,
    /// Binds the accepted catalog and first-run lock from the project's committed
    /// store, so analysis checks source against the committed identity rather than
    /// always re-deriving fresh first-run ids. Keyed on the store's monotonic commit
    /// fact, it skips the full catalog-snapshot read when the store has not advanced,
    /// keeping the per-debounce recompute cheap.
    catalog_binding: CatalogBindingCache,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BindingIndexKey {
    root: PathBuf,
    generation: AnalysisGeneration,
}

impl BindingIndexKey {
    fn new(project: &Project, snapshot: &AnalysisSnapshot) -> Self {
        Self {
            root: project.root.clone(),
            generation: snapshot.generation(),
        }
    }
}

/// A parse-clean, non-empty analysis: the checker kept every module (no file has a
/// parse error) and the program has content. Only such an analysis is a sound source
/// for hover and navigation reads or for last-good retention.
fn snapshot_is_good(snapshot: &AnalysisSnapshot) -> bool {
    !snapshot.program.modules.is_empty()
        && !snapshot.files.iter().any(|file| file.parsed.has_errors())
}

fn snapshot_contains_file(snapshot: &AnalysisSnapshot, file: &Path) -> bool {
    snapshot.files.iter().any(|analyzed| analyzed.path == file)
}

/// Owned inputs for one project analysis, severed from the [`Workspace`] so the
/// CPU-bound check can run off the async runtime's pump without holding the
/// workspace lock. Prepared under the lock, analyzed with no lock held, then
/// committed back under the lock.
pub struct PreparedRecompute {
    root: PathBuf,
    config: ProjectConfig,
    sources: ProjectSources,
    accepted: Option<CatalogMetadata>,
    lock: Option<CatalogLock>,
    documents_generation: u64,
}

impl PreparedRecompute {
    /// Run the project checker. Pure CPU work with no shared state, safe to run on
    /// a blocking thread while read handlers serve last-good facts.
    pub fn analyze(self) -> Result<AnalysisSnapshot, marrow_project::DiscoverError> {
        analyze_project(
            &self.root,
            &self.config,
            &self.sources,
            self.accepted.as_ref(),
            self.lock.as_ref(),
        )
    }

    /// The document generation these inputs were captured at, carried through to
    /// the commit so freshness stays keyed to the analyzed source world.
    pub fn documents_generation(&self) -> u64 {
        self.documents_generation
    }
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    /// The most recent analysis, if any, so a later request can read the cached
    /// snapshot without recomputing. This always reflects the latest recompute —
    /// diagnostics publish from it.
    pub fn latest(&self) -> Option<&AnalysisSnapshot> {
        self.latest.as_ref()
    }

    /// The resolved project, once a buffer has fixed one. Store-inspection
    /// features read the store path and config from it without re-resolving.
    pub fn project(&self) -> Option<&Project> {
        self.project.as_ref()
    }

    /// The latest non-empty program, falling back to the previous non-empty one.
    /// LSP request paths that expose project facts use the fresh accessors below
    /// instead; this fallback is for callers that explicitly choose last-good
    /// behavior after establishing their own recompute boundary.
    pub fn program(&self) -> Option<&CheckedProgram> {
        match self.latest.as_ref() {
            Some(snapshot) if !snapshot.program.modules.is_empty() => Some(&snapshot.program),
            _ => self.last_program.as_ref(),
        }
    }

    /// The binding index for the latest snapshot, built once on first access and
    /// cached until the next recompute. Returns `None` only when no analysis has
    /// run yet. Navigation requests (definition, references, rename) read this
    /// instead of rebuilding the index — the build walks every file's declarations
    /// and bodies, so doing it per request would be wasteful.
    pub fn binding_index(&mut self) -> Option<&BindingIndex> {
        if self.binding_index.is_none() {
            let snapshot = self.latest.as_ref()?;
            let project = self.project.as_ref()?;
            self.binding_index_key = Some(BindingIndexKey::new(project, snapshot));
            self.binding_index = Some(build_binding_index(snapshot));
        }
        self.binding_index.as_ref()
    }

    /// The binding index if it has already been built, without building it. Lets a
    /// caller take a read-only borrow of the cached index alongside another
    /// read-only borrow of the snapshot, after [`Self::binding_index`] has populated
    /// it — the lazy builder needs `&mut self`, which cannot coexist with those.
    pub fn binding_index_cached(&self) -> Option<&BindingIndex> {
        self.binding_index.as_ref()
    }

    /// Whether the latest snapshot still matches the editor-visible source world.
    pub fn latest_matches_sources(&self, documents: &Documents) -> bool {
        self.latest_matches_snapshot(&documents.analysis_snapshot())
    }

    /// Whether the latest snapshot still matches a captured editor-visible source world.
    ///
    /// An O(1) generation compare: any open, edit, or close bumps the document
    /// generation, so an unchanged generation guarantees every open buffer is
    /// identical to the one the snapshot was analyzed from. External changes to
    /// closed files on disk are picked up by the file watcher's invalidation, not by
    /// re-reading the project on every request.
    pub fn latest_matches_snapshot(&self, documents: &DocumentAnalysisSnapshot) -> bool {
        self.latest.is_some() && self.latest_documents_generation == Some(documents.generation())
    }

    /// The latest snapshot only when it still matches the editor-visible sources.
    pub fn fresh_latest(&self, documents: &Documents) -> Option<&AnalysisSnapshot> {
        self.latest_matches_snapshot(&documents.analysis_snapshot())
            .then_some(self.latest.as_ref()?)
    }

    pub fn fresh_latest_from_snapshot(
        &self,
        documents: &DocumentAnalysisSnapshot,
    ) -> Option<&AnalysisSnapshot> {
        self.latest_matches_snapshot(documents)
            .then_some(self.latest.as_ref()?)
    }

    /// The current checked program only when it belongs to the fresh snapshot.
    pub fn fresh_program(&self, documents: &Documents) -> Option<&CheckedProgram> {
        self.fresh_latest_from_snapshot(&documents.analysis_snapshot())
            .map(|snapshot| &snapshot.program)
            .filter(|program| !program.modules.is_empty())
    }

    pub fn fresh_program_from_snapshot(
        &self,
        documents: &DocumentAnalysisSnapshot,
    ) -> Option<&CheckedProgram> {
        self.fresh_latest_from_snapshot(documents)
            .map(|snapshot| &snapshot.program)
            .filter(|program| !program.modules.is_empty())
    }

    /// The latest snapshot's program, even when empty, when the snapshot is fresh.
    pub fn fresh_program_or_empty(&self, documents: &Documents) -> Option<&CheckedProgram> {
        self.fresh_latest_from_snapshot(&documents.analysis_snapshot())
            .map(|snapshot| &snapshot.program)
    }

    pub fn fresh_program_or_empty_from_snapshot(
        &self,
        documents: &DocumentAnalysisSnapshot,
    ) -> Option<&CheckedProgram> {
        self.fresh_latest_from_snapshot(documents)
            .map(|snapshot| &snapshot.program)
    }

    /// Resolve the project for `file`, overlay every open buffer under that root,
    /// run the checker, cache the snapshot, and return a reference to it.
    ///
    /// The cached project is reused only while `file` still lives under its root.
    /// Multi-root editor sessions can open unrelated Marrow projects in one server
    /// process, so a later file outside the current root resolves its own nearest
    /// `marrow.json` instead of being analyzed against the first project. A file
    /// outside any project yields [`WorkspaceError::NoProject`].
    pub fn recompute(
        &mut self,
        file: &Path,
        documents: &Documents,
    ) -> Result<&AnalysisSnapshot, WorkspaceError> {
        self.recompute_from_snapshot(file, &documents.analysis_snapshot())
    }

    pub fn recompute_from_snapshot(
        &mut self,
        file: &Path,
        documents: &DocumentAnalysisSnapshot,
    ) -> Result<&AnalysisSnapshot, WorkspaceError> {
        let prepared = self.prepare_recompute(file, documents)?;
        let generation = prepared.documents_generation();
        let snapshot = prepared.analyze().map_err(WorkspaceError::Discover)?;
        self.commit_recompute(generation, snapshot);
        Ok(self.latest.as_ref().expect("commit stored a snapshot"))
    }

    /// Resolve the project, bind its committed catalog, and overlay the open
    /// buffers into an owned analysis job the caller runs off the lock. The project
    /// and catalog-binding cache are updated here, under the caller's lock.
    pub fn prepare_recompute(
        &mut self,
        file: &Path,
        documents: &DocumentAnalysisSnapshot,
    ) -> Result<PreparedRecompute, WorkspaceError> {
        if !self
            .project
            .as_ref()
            .is_some_and(|project| file.starts_with(&project.root))
        {
            self.project = Some(resolve_project(file)?);
            self.catalog_binding = CatalogBindingCache::new();
        }
        let project = self.project.as_ref().expect("project resolved just above");
        let sources = overlay_snapshot(&project.root, documents);
        let binding = self
            .catalog_binding
            .bind(&project.root, &project.config)
            .map_err(WorkspaceError::AnalysisContext)?;
        Ok(PreparedRecompute {
            root: project.root.clone(),
            config: project.config.clone(),
            sources,
            accepted: binding.accepted,
            lock: binding.lock,
            documents_generation: documents.generation(),
        })
    }

    /// Store a freshly analyzed snapshot as the latest analysis.
    pub fn commit_recompute(&mut self, documents_generation: u64, snapshot: AnalysisSnapshot) {
        let project = self
            .project
            .as_ref()
            .expect("prepared a project to analyze");
        let binding_index_key = BindingIndexKey::new(project, &snapshot);

        // Retain the last program that had modules, so a later recompute whose
        // active buffer errors (dropping its module) does not erase the schema and
        // signatures completion draws on.
        if !snapshot.program.modules.is_empty() {
            self.last_program = Some(snapshot.program.clone());
        }

        let keep_binding_index = self.binding_index_key.as_ref() == Some(&binding_index_key);
        self.latest = Some(snapshot);
        if !keep_binding_index {
            self.binding_index = None;
            self.binding_index_key = None;
        }
        self.latest_documents_generation = Some(documents_generation);
    }

    /// Whether the latest analysis can answer a hover or navigation read for `file`:
    /// it is parse-clean, non-empty, and analyzed that file. Builds and caches its
    /// binding index so the immutable read that follows can borrow it. The analysis
    /// may be prior-generation while a recompute for newer source is in flight; the
    /// caller reads it as best-effort facts, and a finished recompute swaps in the
    /// new snapshot atomically.
    pub fn latest_reads_file(&mut self, file: &Path) -> bool {
        let readable = self.latest.as_ref().is_some_and(|snapshot| {
            snapshot_is_good(snapshot) && snapshot_contains_file(snapshot, file)
        });
        if readable {
            self.binding_index();
        }
        readable
    }

    /// Drop cached checked facts while keeping the resolved project config.
    pub fn invalidate_analysis(&mut self) {
        self.latest = None;
        self.last_program = None;
        self.binding_index = None;
        self.binding_index_key = None;
        self.latest_documents_generation = None;
    }

    /// Drop cached checked facts and force the next recompute to re-read config.
    pub fn invalidate_project(&mut self) {
        self.project = None;
        self.invalidate_analysis();
    }
}

/// Walk up from `file` to the nearest directory holding a `marrow.json`, parse it,
/// and return the project rooted there.
fn resolve_project(file: &Path) -> Result<Project, WorkspaceError> {
    let mut directory = file.parent();
    while let Some(current) = directory {
        let config_path = current.join(CONFIG_FILE);
        if config_path.is_file() {
            let text = std::fs::read_to_string(&config_path).map_err(|error| {
                WorkspaceError::Config(marrow_project::ConfigError {
                    code: marrow_project::CONFIG_INVALID,
                    kind: marrow_project::ConfigErrorKind::InvalidJson,
                    message: error.to_string(),
                    position: None,
                })
            })?;
            let config = parse_config(&text).map_err(WorkspaceError::Config)?;
            return Ok(Project {
                root: current.to_path_buf(),
                config,
            });
        }
        directory = current.parent();
    }
    Err(WorkspaceError::NoProject)
}

/// Build a source overlay from every open buffer whose file lives under `root`.
/// A buffer outside the project is left out so it cannot perturb the analysis.
fn overlay_snapshot(root: &Path, documents: &DocumentAnalysisSnapshot) -> ProjectSources {
    let mut sources = ProjectSources::new();
    for document in documents.open_documents() {
        if document.path.starts_with(root) {
            sources.insert(document.path.clone(), document.text.clone());
        }
    }
    sources
}

/// The filesystem path of a `file://` URL, or `None` for any other scheme.
pub fn url_to_path(url: &Url) -> Option<PathBuf> {
    url.to_file_path().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{path_to_url, snapshot_to_diagnostics};
    use marrow_store::tree::StoreUid;
    use marrow_store::{AccessMode, SealedStore};

    /// A native-store project carrying a stamped store with a committed catalog
    /// baseline. Returns the project root path inside the kept `TempDir` so a recompute
    /// observes the committed accepted identity.
    fn stamped_native_project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("counter.mw"),
            "module counter\n\nresource Counter\n    required value: int\n\n\
             store ^counter(id: int): Counter\n",
        )
        .unwrap();

        let config = marrow_check::load_config(&root).unwrap();
        let program = marrow_check::check_project_against(&root, &config, None, None).unwrap();

        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let store = SealedStore::open(&data_dir.join("marrow.redb"), AccessMode::Create)
            .unwrap()
            .into_store();
        store
            .write_store_uid(&StoreUid::new("store_00000000000000000000000000000001").unwrap())
            .unwrap();
        marrow_run::evolution::commit_catalog_baseline(&store, &program).unwrap();
        drop(store);

        (dir, root)
    }

    /// The editor must check against the store's committed accepted identity, not a
    /// fresh first run. A stamped store carries an accepted catalog epoch; the storeless
    /// bug analyzed every project as a first run, leaving the accepted catalog `None`.
    #[test]
    fn recompute_reflects_the_committed_accepted_identity_of_a_stamped_store() {
        let (_dir, root) = stamped_native_project();
        let file = root.join("src/counter.mw");

        let mut workspace = Workspace::new();
        let documents = Documents::new();
        let snapshot = workspace.recompute(&file, &documents).unwrap();

        assert!(
            snapshot.generation().accepted_catalog.is_some(),
            "a stamped store must bind its committed accepted catalog, not be analyzed as a \
             fresh first run"
        );
        assert!(
            snapshot.program.catalog.accepted_epoch.is_some(),
            "the checked program must carry the store's committed accepted epoch"
        );
    }

    #[test]
    fn recompute_accepts_omitted_store_for_storeless_project() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("app.mw");
        std::fs::write(&file, "module app\n\npub fn main()\n    print(\"hi\")\n").unwrap();

        let mut workspace = Workspace::new();
        let documents = Documents::new();
        let (diagnostics_empty, diagnostics) = {
            let snapshot = workspace.recompute(&file, &documents).unwrap();
            (
                snapshot.report.diagnostics.is_empty(),
                format!("{:?}", snapshot.report.diagnostics),
            )
        };

        assert_eq!(
            workspace.project().unwrap().config.store.backend,
            marrow_project::StoreBackend::Memory
        );
        assert!(
            diagnostics_empty,
            "storeless source project should recompute without diagnostics, got {diagnostics}"
        );
    }

    /// A project whose one library file is clean on disk; an open buffer overlays a
    /// version with a type error. Analysis must report the error against the buffer
    /// while the same project checked from disk alone reports none.
    #[test]
    fn open_buffer_overlay_surfaces_an_error_the_disk_file_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("a.mw");
        // A clean module: a value-returning function that returns an int.
        std::fs::write(&file, "module a\n\npub fn answer(): int\n    return 42\n").unwrap();

        // From disk alone, no diagnostics.
        let mut clean = Workspace::new();
        let empty = Documents::new();
        let clean_snapshot = clean.recompute(&file, &empty).unwrap();
        assert!(
            clean_snapshot.report.diagnostics.is_empty(),
            "clean on-disk project should have no diagnostics, got {:?}",
            clean_snapshot.report.diagnostics
        );

        // Overlay a buffer whose body returns a string where an int is declared.
        let url = path_to_url(&file).unwrap();
        let mut documents = Documents::new();
        documents.open(
            url.clone(),
            "module a\n\npub fn answer(): int\n    return \"nope\"\n".to_string(),
        );

        let mut workspace = Workspace::new();
        let snapshot = workspace.recompute(&file, &documents).unwrap();
        assert!(
            !snapshot.report.diagnostics.is_empty(),
            "overlay with a type error should report a diagnostic"
        );

        // The error is published for the buffer's URL.
        let urls: Vec<Url> = documents.urls().cloned().collect();
        let published =
            snapshot_to_diagnostics(snapshot, &urls, |u| documents.get(u).map(|d| &d.index));
        let diagnostics = &published[&url];
        assert!(
            diagnostics.iter().any(|d| matches!(
                &d.code,
                Some(lsp_types::NumberOrString::String(code)) if code == "check.return_type"
            )),
            "expected a check.return_type diagnostic, got {diagnostics:?}"
        );
    }

    #[test]
    fn project_analysis_membership_includes_source_roots_and_configured_tests() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let project = Project {
            root: root.to_path_buf(),
            config: parse_config(
                r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" }, "tests": ["tests"] }"#,
            )
            .unwrap(),
        };

        assert!(project.analyzes_file(&root.join("src/app.mw")));
        assert!(project.analyzes_file(&root.join("src/a.b.mw")));
        assert!(project.analyzes_file(&root.join("tests/deep/app_test.mw")));
        assert!(!project.analyzes_file(&root.join("notes/app.mw")));
        assert!(!project.analyzes_file(&root.join("src/app.txt")));
    }

    #[test]
    fn latest_open_document_match_rejects_a_closed_analysis_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let defs = src.join("defs.mw");
        let app = src.join("app.mw");
        std::fs::write(&defs, "module defs\n\npub fn answer(): int\n    return 1\n").unwrap();
        std::fs::write(
            &app,
            "module app\nuse defs\n\npub fn call(): int\n    return defs::answer()\n",
        )
        .unwrap();

        let defs_url = path_to_url(&defs).unwrap();
        let app_url = path_to_url(&app).unwrap();
        let mut documents = Documents::new();
        documents.open(
            defs_url.clone(),
            "module defs\n\npub fn renamed(): int\n    return 1\n".to_string(),
        );
        documents.open(
            app_url,
            "module app\nuse defs\n\npub fn call(): int\n    return defs::renamed()\n".to_string(),
        );

        let mut workspace = Workspace::new();
        workspace.recompute(&app, &documents).unwrap();
        assert!(workspace.latest_matches_sources(&documents));

        documents.close(&defs_url);
        assert!(
            !workspace.latest_matches_sources(&documents),
            "closing an analyzed overlay changes the source set backing the snapshot"
        );
    }

    #[test]
    fn external_closed_file_changes_refresh_via_invalidation_not_per_request() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let app = src.join("app.mw");
        let schema = src.join("schema.mw");
        std::fs::write(&app, "module app\n\npub fn f(): int\n    return 1\n").unwrap();
        std::fs::write(&schema, "module schema\n\npub fn g(): int\n    return 1\n").unwrap();

        let app_url = path_to_url(&app).unwrap();
        let mut documents = Documents::new();
        documents.open(
            app_url,
            "module app\n\npub fn f(): int\n    return 1\n".to_string(),
        );

        let mut workspace = Workspace::new();
        workspace.recompute(&app, &documents).unwrap();
        assert!(workspace.latest_matches_sources(&documents));

        // A closed file changing on disk does not bump the document generation, so the
        // freshness check does not re-read the project and the snapshot stays fresh. This is
        // the deliberate trade for an O(1) hot path: the file watcher, not a per-request disk
        // walk, refreshes external changes. Adding or deleting a closed file behaves the same.
        std::fs::write(
            &schema,
            "module schema\n\npub fn changed(): int\n    return 1\n",
        )
        .unwrap();
        std::fs::write(
            src.join("added.mw"),
            "module added\n\npub fn h(): int\n    return 1\n",
        )
        .unwrap();
        assert!(
            workspace.latest_matches_sources(&documents),
            "an external change to a closed file is not detected per request"
        );

        // The `**/*.mw` watcher fires and invalidates the analysis; the next freshness check
        // is then stale and forces a recompute that picks the external change up.
        workspace.invalidate_analysis();
        assert!(
            !workspace.latest_matches_sources(&documents),
            "invalidation from the file watcher refreshes external changes"
        );
    }

    #[test]
    fn a_file_outside_any_project_has_no_project() {
        let dir = tempfile::tempdir().unwrap();
        let stray = dir.path().join("loose.mw");
        std::fs::write(&stray, "module loose\n").unwrap();
        let mut workspace = Workspace::new();
        let documents = Documents::new();
        assert!(matches!(
            workspace.recompute(&stray, &documents),
            Err(WorkspaceError::NoProject)
        ));
    }

    #[test]
    fn recompute_switches_projects_when_the_file_moves_outside_the_cached_root() {
        fn project(dir: &tempfile::TempDir, module: &str) -> PathBuf {
            let root = dir.path();
            std::fs::write(
                root.join("marrow.json"),
                r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
            )
            .unwrap();
            let src = root.join("src");
            std::fs::create_dir_all(&src).unwrap();
            let file = src.join(format!("{module}.mw"));
            std::fs::write(
                &file,
                format!("module {module}\n\npub fn id(): int\n    return 1\n"),
            )
            .unwrap();
            file
        }

        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let first = project(&first_dir, "first");
        let second = project(&second_dir, "second");
        let documents = Documents::new();
        let mut workspace = Workspace::new();

        workspace.recompute(&first, &documents).unwrap();
        let snapshot = workspace.recompute(&second, &documents).unwrap();

        assert!(
            snapshot.files.iter().any(|file| file.path == second),
            "the second project should be analyzed, not the cached first project"
        );
        assert_eq!(
            workspace.project().map(|project| project.root.as_path()),
            Some(second_dir.path())
        );
    }

    #[test]
    fn binding_index_survives_same_root_same_analysis_identity_recompute() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("app.mw");
        let source = "module app\n\npub fn answer(): int\n    return 1\n";
        std::fs::write(&file, source).unwrap();

        let mut workspace = Workspace::new();
        let documents = Documents::new();
        workspace.recompute(&file, &documents).unwrap();
        let first_identity = workspace.latest().unwrap().content_identity().clone();
        workspace.binding_index().unwrap();

        workspace.recompute(&file, &documents).unwrap();
        let second_identity = workspace.latest().unwrap().content_identity();

        assert_eq!(
            second_identity, &first_identity,
            "same root and unchanged source should keep the same analysis identity"
        );
        assert!(
            workspace.binding_index.is_some(),
            "unchanged analysis generation should keep the derived binding index"
        );
    }

    #[test]
    fn binding_index_clears_when_analysis_identity_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("app.mw");
        std::fs::write(&file, "module app\n\npub fn answer(): int\n    return 1\n").unwrap();

        let mut workspace = Workspace::new();
        let documents = Documents::new();
        workspace.recompute(&file, &documents).unwrap();
        let first_identity = workspace.latest().unwrap().content_identity().clone();
        workspace.binding_index().unwrap();

        std::fs::write(&file, "module app\n\npub fn answer(): int\n    return 2\n").unwrap();
        workspace.recompute(&file, &documents).unwrap();
        let second_identity = workspace.latest().unwrap().content_identity();

        assert_ne!(
            second_identity, &first_identity,
            "changed source should produce a different analysis identity"
        );
        assert!(
            workspace.binding_index.is_none(),
            "changed analysis generation should discard the derived binding index"
        );
    }

    #[test]
    fn binding_index_clears_when_analysis_generation_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": ".data" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("app.mw");
        std::fs::write(
            &file,
            "module app\n\nresource Book\n    title: string\n\nstore ^books(id: int): Book\n",
        )
        .unwrap();

        let mut workspace = Workspace::new();
        let documents = Documents::new();
        workspace.recompute(&file, &documents).unwrap();
        let first_generation = workspace.latest().unwrap().generation();
        let proposal = workspace
            .latest()
            .unwrap()
            .program
            .catalog
            .proposal
            .as_ref()
            .expect("native resource analysis proposes catalog identity")
            .clone();
        let source_digest = workspace.latest().unwrap().program.source_digest();
        workspace.binding_index().unwrap();

        marrow_check::project_store_lock(root, &proposal, &source_digest).unwrap();
        workspace.recompute(&file, &documents).unwrap();
        let second_generation = workspace.latest().unwrap().generation();

        assert_eq!(
            second_generation.content_identity, first_generation.content_identity,
            "committing catalog generation leaves analyzed source identity unchanged"
        );
        assert_ne!(
            second_generation, first_generation,
            "committing catalog generation should change canonical analysis generation"
        );
        assert!(
            workspace.binding_index.is_none(),
            "changed analysis generation should discard the derived binding index"
        );
    }

    #[test]
    fn binding_index_clears_when_project_root_changes() {
        let dir = tempfile::tempdir().unwrap();
        let first_root = dir.path().join("first");
        let second_root = dir.path().join("second");
        for root in [&first_root, &second_root] {
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(
                root.join("marrow.json"),
                r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
            )
            .unwrap();
            std::fs::write(
                root.join("src/app.mw"),
                "module app\n\npub fn answer(): int\n    return 1\n",
            )
            .unwrap();
        }

        let mut workspace = Workspace::new();
        let documents = Documents::new();
        workspace
            .recompute(&first_root.join("src/app.mw"), &documents)
            .unwrap();
        let first_identity = workspace.latest().unwrap().content_identity().clone();
        workspace.binding_index().unwrap();

        workspace
            .recompute(&second_root.join("src/app.mw"), &documents)
            .unwrap();
        let second_identity = workspace.latest().unwrap().content_identity();

        assert_eq!(
            second_identity, &first_identity,
            "matching source under different roots should keep the same content identity"
        );
        assert!(
            workspace.binding_index.is_none(),
            "same content identity under a different root must not reuse pathful binding facts"
        );
    }
}
