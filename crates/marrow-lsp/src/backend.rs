//! The language server backend: open buffers, the project workspace, and the
//! edit-driven recompute that publishes diagnostics.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use marrow_lsp_core::data_explorer::{
    DataChildViewsResponse, DataChildrenError, DataChildrenRequest, DataKeyDto, DataPresenceDto,
    DataReadRequest, DataReadResponse, DataRequestValidationError, SavedRootsResult, saved_roots,
    session_data_child_views_page, session_data_read, validate_data_children_request,
    validate_data_read_request,
};
use marrow_lsp_core::diagnostics::snapshot_to_diagnostics;
use marrow_lsp_core::documents::{DocumentAnalysisSnapshot, DocumentSnapshot, Documents};
use marrow_lsp_core::navigation::{RenameError, SnapshotIndices};
use marrow_lsp_core::positions::Position as CorePosition;
use marrow_lsp_core::store::{
    SavedDataSession, SavedDataWatchTargets, open_saved_data_session, saved_data_watch_targets,
};
use marrow_lsp_core::workspace::{AnalysisSnapshot, Project, Workspace, url_to_path};
use marrow_lsp_core::{
    completion, formatting, hover, indentation, navigation, semantic_tokens, signature_help,
    symbols,
};
#[cfg(test)]
use tokio::sync::Barrier;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, jsonrpc};

/// How long to wait after an edit before checking the project, so a burst of
/// keystrokes settles into one recompute.
const DEBOUNCE: Duration = Duration::from_millis(700);
const PENDING_ANALYSIS_INVALIDATION: u8 = 0b01;
const PENDING_PROJECT_INVALIDATION: u8 = 0b10;

pub struct Backend {
    client: Client,
    documents: Arc<Mutex<Documents>>,
    analysis: Arc<Mutex<AnalysisState>>,
    diagnostics: Arc<Mutex<DiagnosticsTracker>>,
    diagnostics_publish: Arc<Mutex<()>>,
    /// Bumped for edits whose project root is not known yet. Once a root is known,
    /// diagnostics use the root-scoped epoch instead, so unrelated projects cannot
    /// cancel each other's pending clears.
    edit_seq: Arc<AtomicU64>,
    /// Whether to read real stored data for saved-data requests.
    /// Mirrors the `marrow.liveData` setting, default off; enabling it permits
    /// the server to open the native dev store read-only.
    live_data: Arc<AtomicBool>,
    pending_workspace_invalidation: Arc<AtomicU8>,
}

struct AnalysisState {
    workspace: Workspace,
    #[cfg(test)]
    recompute_gate: Option<Arc<TestRecomputeGate>>,
}

#[cfg(test)]
struct TestRecomputeGate {
    entered: Barrier,
    release: Barrier,
}

#[cfg(test)]
impl TestRecomputeGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Barrier::new(2),
            release: Barrier::new(2),
        })
    }

    async fn wait_until_entered(&self) {
        self.entered.wait().await;
    }

    async fn release(&self) {
        self.release.wait().await;
    }

    async fn hold(&self) {
        self.entered.wait().await;
        self.release.wait().await;
    }
}

#[derive(Default)]
struct DiagnosticsTracker {
    roots: HashMap<PathBuf, RootDiagnostics>,
}

#[derive(Default)]
struct RootDiagnostics {
    epoch: u64,
    non_empty_urls: HashSet<Url>,
}

impl DiagnosticsTracker {
    fn new() -> Self {
        Self::default()
    }

    fn bump_root_epoch(&mut self, root: &Path) -> u64 {
        let diagnostics = self.roots.entry(root.to_path_buf()).or_default();
        diagnostics.epoch += 1;
        diagnostics.epoch
    }

    fn epoch(&self, root: &Path) -> u64 {
        self.roots
            .get(root)
            .map(|diagnostics| diagnostics.epoch)
            .unwrap_or(0)
    }

    fn is_stale(&self, root: &Path, epoch: u64) -> bool {
        self.epoch(root) != epoch
    }

    fn clear_root(&mut self, root: &Path) -> Vec<Url> {
        let Some(diagnostics) = self.roots.get_mut(root) else {
            return Vec::new();
        };
        sorted_urls(std::mem::take(&mut diagnostics.non_empty_urls))
    }

    fn clear_url(&mut self, root: &Path, url: &Url) -> bool {
        self.roots
            .get_mut(root)
            .is_some_and(|diagnostics| diagnostics.non_empty_urls.remove(url))
    }

    #[cfg(test)]
    fn is_tracked(&self, root: &Path, url: &Url) -> bool {
        self.roots
            .get(root)
            .is_some_and(|diagnostics| diagnostics.non_empty_urls.contains(url))
    }

    fn reconcile_success(
        &mut self,
        root: &Path,
        epoch: u64,
        diagnostics: &mut HashMap<Url, Vec<Diagnostic>>,
    ) -> bool {
        if self.is_stale(root, epoch) {
            return false;
        }
        if let Some(root_diagnostics) = self.roots.get(root) {
            for url in &root_diagnostics.non_empty_urls {
                diagnostics.entry(url.clone()).or_default();
            }
        }
        let non_empty_urls = diagnostics
            .iter()
            .filter(|(_, diagnostics)| !diagnostics.is_empty())
            .map(|(url, _)| url.clone())
            .collect();
        self.roots
            .entry(root.to_path_buf())
            .or_default()
            .non_empty_urls = non_empty_urls;
        true
    }

    fn tracked_root_for_path(&self, path: &Path) -> Option<PathBuf> {
        self.roots
            .keys()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .cloned()
    }

    fn tracked_root_for_url(&self, url: &Url) -> Option<PathBuf> {
        self.roots
            .iter()
            .find(|(_, diagnostics)| diagnostics.non_empty_urls.contains(url))
            .map(|(root, _)| root.clone())
    }

    fn tracked_root_for_config_path(&self, path: &Path) -> Option<PathBuf> {
        self.roots
            .keys()
            .find(|root| path == root.join("marrow.json"))
            .cloned()
    }
}

fn sorted_urls(urls: HashSet<Url>) -> Vec<Url> {
    let mut urls: Vec<Url> = urls.into_iter().collect();
    urls.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    urls
}

struct PublishedDiagnostics {
    url: Url,
    diagnostics: Vec<Diagnostic>,
    version: Option<i32>,
}

struct RecomputeContext<'a> {
    documents: &'a Mutex<Documents>,
    analysis: &'a Mutex<AnalysisState>,
    diagnostics: &'a Mutex<DiagnosticsTracker>,
    diagnostics_publish: &'a Mutex<()>,
    pending_workspace_invalidation: &'a AtomicU8,
    client: &'a Client,
}

fn open_analysis_document_urls(
    documents: &DocumentAnalysisSnapshot,
    project: &Project,
) -> Vec<Url> {
    let mut urls: Vec<Url> = documents
        .open_documents()
        .iter()
        .filter(|document| project.analyzes_file(&document.path))
        .map(|document| document.url.clone())
        .collect();
    urls.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    urls
}

fn open_document_versions(documents: &DocumentAnalysisSnapshot) -> HashMap<Url, Option<i32>> {
    documents
        .open_documents()
        .iter()
        .map(|document| (document.url.clone(), document.version))
        .collect()
}

fn clear_publish(url: Url, versions: &HashMap<Url, Option<i32>>) -> PublishedDiagnostics {
    PublishedDiagnostics {
        version: versions.get(&url).copied().flatten(),
        url,
        diagnostics: Vec::new(),
    }
}

fn known_root_for_path(
    workspace: Option<&Workspace>,
    diagnostics: &DiagnosticsTracker,
    path: &Path,
) -> Option<PathBuf> {
    workspace
        .and_then(|workspace| {
            workspace
                .project()
                .filter(|project| path.starts_with(&project.root))
                .map(|project| project.root.clone())
        })
        .or_else(|| diagnostics.tracked_root_for_path(path))
}

fn apply_pending_workspace_invalidation(bits: u8, workspace: &mut Workspace) {
    if bits & PENDING_PROJECT_INVALIDATION != 0 {
        workspace.invalidate_project();
    } else if bits & PENDING_ANALYSIS_INVALIDATION != 0 {
        workspace.invalidate_analysis();
    }
}

async fn snapshot_documents(documents: &Mutex<Documents>) -> DocumentAnalysisSnapshot {
    documents.lock().await.analysis_snapshot()
}

async fn snapshot_document_and_analysis(
    documents: &Mutex<Documents>,
    url: &Url,
) -> Option<(DocumentSnapshot, DocumentAnalysisSnapshot)> {
    let documents = documents.lock().await;
    Some((documents.snapshot(url)?, documents.analysis_snapshot()))
}

async fn documents_match_analysis_snapshot(
    documents: &Mutex<Documents>,
    snapshot: &DocumentAnalysisSnapshot,
) -> bool {
    documents.lock().await.matches_generation(snapshot)
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(Mutex::new(Documents::new())),
            analysis: Arc::new(Mutex::new(AnalysisState {
                workspace: Workspace::new(),
                #[cfg(test)]
                recompute_gate: None,
            })),
            diagnostics: Arc::new(Mutex::new(DiagnosticsTracker::new())),
            diagnostics_publish: Arc::new(Mutex::new(())),
            edit_seq: Arc::new(AtomicU64::new(0)),
            live_data: Arc::new(AtomicBool::new(false)),
            pending_workspace_invalidation: Arc::new(AtomicU8::new(0)),
        }
    }

    /// The Marrow read session for saved-data inspection, only when the cached
    /// analysis still matches the editor-visible source.
    fn data_session(
        &self,
        analysis: &AnalysisState,
        documents: &DocumentAnalysisSnapshot,
    ) -> Option<SavedDataSession> {
        if !self.live_data.load(Ordering::Relaxed) {
            return None;
        }
        if self.workspace_invalidation_pending() {
            return None;
        }
        let fresh_program = analysis.workspace.fresh_program_from_snapshot(documents)?;
        open_saved_data_session(analysis.workspace.project()?)
            .ok()
            .flatten()
            .filter(|session| session.matches_checked_program(fresh_program))
    }

    fn workspace_invalidation_pending(&self) -> bool {
        self.pending_workspace_invalidation.load(Ordering::SeqCst) != 0
    }

    /// `marrow/savedRoots`: the project's saved root views. Requires a fresh
    /// checked program and never recomputes the project.
    pub async fn saved_roots(&self) -> jsonrpc::Result<SavedRootsResult> {
        let documents = snapshot_documents(&self.documents).await;
        let Ok(analysis) = self.analysis.try_lock() else {
            return Ok(saved_roots(None));
        };
        let session = self.data_session(&analysis, &documents);
        Ok(saved_roots(session.as_ref()))
    }

    /// `marrow/dataChildren`: a bounded page of saved-data children for the
    /// committed-store inspector. Requires a fresh editor-visible analysis and a
    /// Marrow saved-data session; unavailable stores degrade to a typed empty response.
    pub async fn data_children(
        &self,
        request: DataChildrenRequest,
    ) -> jsonrpc::Result<DataChildViewsResponse> {
        let request = validate_data_children_request(request)
            .map_err(|error| invalid_data_request("marrow/dataChildren", error))?;
        let documents = snapshot_documents(&self.documents).await;
        let Ok(analysis) = self.analysis.try_lock() else {
            return Ok(unavailable_data_children());
        };
        let Some(session) = self.data_session(&analysis, &documents) else {
            return Ok(unavailable_data_children());
        };
        Ok(match session_data_child_views_page(&session, request) {
            Ok(result) => DataChildViewsResponse {
                available: true,
                children: result.children,
                truncated: result.truncated,
                cursor: result.cursor,
                store_snapshot: result.store_snapshot,
                data_view_boundary: result.data_view_boundary,
                error: None,
            },
            Err(error) => data_children_error(error),
        })
    }

    /// `marrow/dataRead`: read and render one saved-data path for the
    /// committed-store inspector.
    pub async fn data_read(&self, request: DataReadRequest) -> jsonrpc::Result<DataReadResponse> {
        let request = validate_data_read_request(request)
            .map_err(|error| invalid_data_request("marrow/dataRead", error))?;
        let documents = snapshot_documents(&self.documents).await;
        let Ok(analysis) = self.analysis.try_lock() else {
            return Ok(unavailable_data_read());
        };
        let Some(session) = self.data_session(&analysis, &documents) else {
            return Ok(unavailable_data_read());
        };
        Ok(match session_data_read(&session, request) {
            Ok(result) => DataReadResponse {
                available: true,
                presence: result.presence,
                value: result.value,
                value_truncated: result.value_truncated,
                store_snapshot: result.store_snapshot,
                data_view_boundary: result.data_view_boundary,
                error: None,
            },
            Err(error) => data_read_error(error),
        })
    }

    /// `marrow/dataWatchTargets`: filesystem inputs whose changes can make the
    /// saved-resource inspector stale. Paths are derived from the current
    /// validated project config and remain unavailable while live data is off.
    pub async fn data_watch_targets(&self) -> jsonrpc::Result<SavedDataWatchTargets> {
        let Ok(analysis) = self.analysis.try_lock() else {
            return Ok(SavedDataWatchTargets { paths: Vec::new() });
        };
        if self.workspace_invalidation_pending() {
            return Ok(SavedDataWatchTargets { paths: Vec::new() });
        }
        saved_data_watch_targets(
            analysis.workspace.project(),
            self.live_data.load(Ordering::Relaxed),
        )
        .map_err(|error| {
            jsonrpc::Error::invalid_params(format!("invalid project store config: {error}"))
        })
    }

    /// Note that an edit happened and schedule a debounced recompute for `file`.
    /// The recompute is dropped if another edit lands before the debounce elapses.
    fn schedule_recompute(&self, file: PathBuf, root_epoch: Option<(PathBuf, u64)>) {
        let seq = root_epoch
            .is_none()
            .then(|| self.edit_seq.fetch_add(1, Ordering::SeqCst) + 1);
        let documents = Arc::clone(&self.documents);
        let analysis = Arc::clone(&self.analysis);
        let diagnostics = Arc::clone(&self.diagnostics);
        let edit_seq = Arc::clone(&self.edit_seq);
        let diagnostics_publish = Arc::clone(&self.diagnostics_publish);
        let pending_workspace_invalidation = Arc::clone(&self.pending_workspace_invalidation);
        let client = self.client.clone();
        tokio::spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            match (&root_epoch, seq) {
                // A newer edit for this project root has been scheduled; let it
                // do the work instead. The publish path checks again after
                // recompute, so a race after this preflight is still contained.
                (Some((root, epoch)), _) => {
                    let diagnostics = diagnostics.lock().await;
                    if diagnostics.is_stale(root, *epoch) {
                        return;
                    }
                }
                // The root is still unknown, so retain the old coarse debounce
                // until a recompute resolves the project.
                (None, Some(seq)) if edit_seq.load(Ordering::SeqCst) != seq => return,
                _ => {}
            }
            Self::recompute_and_publish(
                RecomputeContext {
                    documents: &documents,
                    analysis: &analysis,
                    diagnostics: &diagnostics,
                    diagnostics_publish: &diagnostics_publish,
                    pending_workspace_invalidation: &pending_workspace_invalidation,
                    client: &client,
                },
                &file,
                root_epoch,
            )
            .await;
        });
    }

    /// Recompute one project against the current buffers and publish diagnostics
    /// for that root. A superseded root edit is dropped before this runs; the
    /// publish lock keeps a concurrent recompute from interleaving a partial
    /// publish.
    async fn recompute_and_publish(
        context: RecomputeContext<'_>,
        file: &Path,
        root_epoch: Option<(PathBuf, u64)>,
    ) {
        let analysis_snapshot = {
            let documents = context.documents.lock().await;
            documents.analysis_snapshot()
        };
        let mut analysis = context.analysis.lock().await;
        let AnalysisState {
            workspace,
            #[cfg(test)]
            recompute_gate,
        } = &mut *analysis;
        #[cfg(test)]
        if let Some(gate) = recompute_gate.clone() {
            gate.hold().await;
        }
        let pending = context
            .pending_workspace_invalidation
            .swap(0, Ordering::SeqCst);
        apply_pending_workspace_invalidation(pending, workspace);
        match workspace.recompute_from_snapshot(file, &analysis_snapshot) {
            Ok(_) => {
                let publish_guard = context.diagnostics_publish.lock().await;
                let pending = context
                    .pending_workspace_invalidation
                    .swap(0, Ordering::SeqCst);
                if pending != 0 {
                    apply_pending_workspace_invalidation(pending, workspace);
                    return;
                }
                if !documents_match_analysis_snapshot(context.documents, &analysis_snapshot).await {
                    workspace.invalidate_analysis();
                    return;
                }
                let project = workspace
                    .project()
                    .expect("recompute succeeded with a project")
                    .clone();
                let snapshot = workspace
                    .latest()
                    .expect("recompute just stored a snapshot")
                    .clone();
                let root = project.root.clone();
                let urls = open_analysis_document_urls(&analysis_snapshot, &project);
                let versions = open_document_versions(&analysis_snapshot);
                let mut diagnostics_by_url = snapshot_to_diagnostics(&snapshot, &urls, |url| {
                    analysis_snapshot.get(url).map(|document| &document.index)
                });
                drop(analysis);
                if context
                    .pending_workspace_invalidation
                    .load(Ordering::SeqCst)
                    != 0
                {
                    drop(publish_guard);
                    let mut analysis = context.analysis.lock().await;
                    let pending = context
                        .pending_workspace_invalidation
                        .swap(0, Ordering::SeqCst);
                    apply_pending_workspace_invalidation(pending, &mut analysis.workspace);
                    return;
                }
                let published = {
                    let mut diagnostics = context.diagnostics.lock().await;
                    let epoch = root_epoch
                        .as_ref()
                        .filter(|(scheduled_root, _)| scheduled_root == &root)
                        .map(|(_, epoch)| *epoch)
                        .unwrap_or_else(|| diagnostics.epoch(&root));
                    if !diagnostics.reconcile_success(&root, epoch, &mut diagnostics_by_url) {
                        None
                    } else {
                        Some(
                            diagnostics_by_url
                                .into_iter()
                                .map(|(url, diagnostics)| PublishedDiagnostics {
                                    version: versions.get(&url).copied().flatten(),
                                    url,
                                    diagnostics,
                                })
                                .collect::<Vec<_>>(),
                        )
                    }
                };
                if let Some(published) = published {
                    for published in published {
                        context
                            .client
                            .publish_diagnostics(
                                published.url,
                                published.diagnostics,
                                published.version,
                            )
                            .await;
                    }
                }
            }
            // A buffer outside any project, or a project that cannot be walked,
            // has no fresh source diagnostics to publish. Explicit filesystem
            // deletions clear tracked diagnostics at the watcher boundary; a
            // transient config or discovery failure leaves the last diagnostics
            // visible rather than hiding them.
            Err(error) => {
                if !matches!(error, marrow_lsp_core::workspace::WorkspaceError::NoProject) {
                    eprintln!(
                        "{} recompute failed for {}: {error}",
                        crate::stderr_error("marrow-lsp:"),
                        file.display()
                    );
                }
            }
        }
    }
}

fn unavailable_data_children() -> DataChildViewsResponse {
    DataChildViewsResponse {
        available: false,
        children: Vec::new(),
        truncated: false,
        cursor: None::<DataKeyDto>,
        store_snapshot: None,
        data_view_boundary: None,
        error: None,
    }
}

fn data_children_error(error: DataChildrenError) -> DataChildViewsResponse {
    DataChildViewsResponse {
        available: true,
        children: Vec::new(),
        truncated: false,
        cursor: None::<DataKeyDto>,
        store_snapshot: None,
        data_view_boundary: None,
        error: Some(error),
    }
}

fn unavailable_data_read() -> DataReadResponse {
    DataReadResponse {
        available: false,
        presence: DataPresenceDto::Absent,
        value: None,
        value_truncated: false,
        store_snapshot: None,
        data_view_boundary: None,
        error: None,
    }
}

fn data_read_error(error: DataChildrenError) -> DataReadResponse {
    DataReadResponse {
        available: true,
        presence: DataPresenceDto::Absent,
        value: None,
        value_truncated: false,
        store_snapshot: None,
        data_view_boundary: None,
        error: Some(error),
    }
}

fn invalid_data_request(method: &str, error: DataRequestValidationError) -> jsonrpc::Error {
    jsonrpc::Error::invalid_params(format!("invalid {method} params: {}", error.message()))
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        // `marrow.liveData` (default false) gates reading real stored data. The
        // client passes it under `initializationOptions`; only an explicit `true`
        // enables opening the native dev store.
        if let Some(options) = &params.initialization_options
            && options
                .get("marrow.liveData")
                .and_then(|value| value.as_bool())
                == Some(true)
        {
            self.live_data.store(true, Ordering::Relaxed);
        }
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "marrow-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                // Rename advertises a prepare step so the editor can pre-select the
                // identifier and surface a refusal (e.g. a saved field) before
                // prompting for the new name.
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                })),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    // The sigils and separators that open a new completion context;
                    // the core classifies which one from the tokens left of the
                    // cursor. `(` re-triggers inside a labeled-argument list.
                    trigger_characters: Some(
                        ["^", ".", ":", "("].iter().map(|c| c.to_string()).collect(),
                    ),
                    ..CompletionOptions::default()
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(["(", ","].iter().map(|c| c.to_string()).collect()),
                    ..SignatureHelpOptions::default()
                }),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: semantic_tokens::legend(),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            ..SemanticTokensOptions::default()
                        },
                    ),
                ),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_on_type_formatting_provider: Some(DocumentOnTypeFormattingOptions {
                    first_trigger_character: "\n".to_string(),
                    more_trigger_character: None,
                }),
                ..ServerCapabilities::default()
            },
        })
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        Ok(())
    }

    async fn initialized(&self, _: InitializedParams) {
        eprintln!("{} initialized", crate::stderr_notice("marrow-lsp:"));
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        let url = document.uri;
        let path = url_to_path(&url);
        let root_epoch = {
            let _publish_guard = self.diagnostics_publish.lock().await;
            let mut documents = self.documents.lock().await;
            documents.open_with_version(url.clone(), document.text, Some(document.version));
            drop(documents);
            let Some(path) = path.as_ref() else {
                return;
            };
            let analysis = self.analysis.try_lock().ok();
            let mut diagnostics = self.diagnostics.lock().await;
            known_root_for_path(
                analysis.as_ref().map(|analysis| &analysis.workspace),
                &diagnostics,
                path,
            )
            .map(|root| {
                let epoch = diagnostics.bump_root_epoch(&root);
                (root, epoch)
            })
        };
        if let Some(path) = path {
            self.schedule_recompute(path, root_epoch);
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let url = params.text_document.uri;
        let version = params.text_document.version;
        // Full-text sync: the client sends the whole document as a single change.
        let Some(change) = params.content_changes.into_iter().next_back() else {
            return;
        };
        let path = url_to_path(&url);
        let root_epoch = {
            let _publish_guard = self.diagnostics_publish.lock().await;
            let mut documents = self.documents.lock().await;
            documents.change_with_version(&url, change.text, Some(version));
            drop(documents);
            let Some(path) = path.as_ref() else {
                return;
            };
            let analysis = self.analysis.try_lock().ok();
            let mut diagnostics = self.diagnostics.lock().await;
            known_root_for_path(
                analysis.as_ref().map(|analysis| &analysis.workspace),
                &diagnostics,
                path,
            )
            .map(|root| {
                let epoch = diagnostics.bump_root_epoch(&root);
                (root, epoch)
            })
        };
        if let Some(path) = path {
            self.schedule_recompute(path, root_epoch);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let url = params.text_document.uri;
        let path = url_to_path(&url);
        let _publish_guard = self.diagnostics_publish.lock().await;
        {
            let mut documents = self.documents.lock().await;
            documents.close(&url);
        }
        let root_epoch = {
            let analysis = self.analysis.try_lock().ok();
            let mut diagnostics = self.diagnostics.lock().await;
            if let Some(path) = path.as_ref()
                && let Some(root) = known_root_for_path(
                    analysis.as_ref().map(|analysis| &analysis.workspace),
                    &diagnostics,
                    path,
                )
            {
                diagnostics.clear_url(&root, &url);
                let epoch = diagnostics.bump_root_epoch(&root);
                Some((root, epoch))
            } else {
                None
            }
        };
        // A closed buffer's problems no longer apply to the editor's view.
        self.client.publish_diagnostics(url, Vec::new(), None).await;
        if let Some(path) = path {
            self.schedule_recompute(path, root_epoch);
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let changes: Vec<WatchedPathChange> = params
            .changes
            .into_iter()
            .filter_map(|event| {
                url_to_path(&event.uri).map(|path| WatchedPathChange {
                    uri: event.uri,
                    path,
                    typ: event.typ,
                })
            })
            .collect();
        if changes.is_empty() {
            return;
        }

        let _publish_guard = self.diagnostics_publish.lock().await;
        let pre_bits = busy_watched_pre_invalidation_bits(&changes);
        let versions = {
            let documents = self.documents.lock().await;
            open_document_versions(&documents.analysis_snapshot())
        };
        let (schedules, immediate_clears) = match self.analysis.try_lock() {
            Err(_) => {
                if pre_bits != 0 {
                    self.pending_workspace_invalidation
                        .fetch_or(pre_bits, Ordering::SeqCst);
                }
                let effects = {
                    if pre_bits != 0 {
                        self.pending_workspace_invalidation
                            .fetch_or(pre_bits, Ordering::SeqCst);
                    }
                    let mut diagnostics = self.diagnostics.lock().await;
                    let pending =
                        pending_watched_roots(busy_watched_project_changes(&diagnostics, &changes));
                    let effects = watched_root_effects(
                        &mut diagnostics,
                        &versions,
                        pending,
                        None,
                        ClearRootDrain::Schedule,
                    );
                    if effects.bits != 0 {
                        self.pending_workspace_invalidation
                            .fetch_or(effects.bits, Ordering::SeqCst);
                    }
                    effects
                };
                (effects.schedules, effects.immediate_clears)
            }
            Ok(mut analysis) => {
                let pending = self
                    .pending_workspace_invalidation
                    .swap(0, Ordering::SeqCst);
                apply_pending_workspace_invalidation(pending, &mut analysis.workspace);
                let mut diagnostics = self.diagnostics.lock().await;
                let events = watched_project_changes(&analysis.workspace, &changes)
                    .into_iter()
                    .chain(watched_tracked_diagnostic_changes(&diagnostics, &changes))
                    .collect::<Vec<_>>();
                if events.is_empty() {
                    return;
                }
                let current_root = analysis
                    .workspace
                    .project()
                    .map(|project| project.root.clone());
                let pending = pending_watched_roots(events);
                let effects = watched_root_effects(
                    &mut diagnostics,
                    &versions,
                    pending,
                    current_root.as_deref(),
                    ClearRootDrain::Skip,
                );
                if effects.invalidated_project {
                    analysis.workspace.invalidate_project();
                } else if effects.invalidated_analysis {
                    analysis.workspace.invalidate_analysis();
                }
                (effects.schedules, effects.immediate_clears)
            }
        };

        if !immediate_clears.is_empty() {
            for published in immediate_clears {
                self.client
                    .publish_diagnostics(published.url, published.diagnostics, published.version)
                    .await;
            }
        }
        for schedule in schedules {
            self.schedule_recompute(schedule.path, schedule.root_epoch);
        }
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let position = params.text_document_position_params;
        let url = position.text_document.uri;
        let Some(path) = url_to_path(&url) else {
            return Ok(None);
        };

        let Some((document, documents)) =
            snapshot_document_and_analysis(&self.documents, &url).await
        else {
            return Ok(None);
        };
        let offset = document.index.offset(to_core_position(position.position));
        let Ok(mut analysis) = self.analysis.try_lock() else {
            return Ok(None);
        };
        if self.workspace_invalidation_pending() {
            return Ok(None);
        }
        // Build and cache the binding index (once per recompute) so the docs
        // lookup reuses it exactly like definition and references do.
        if fresh_index_if_available(&mut analysis.workspace, &documents, &path, &document.text)
            .is_none()
        {
            return Ok(None);
        }
        let snapshot = analysis.workspace.latest().expect("ensured just above");
        let index = analysis
            .workspace
            .binding_index_cached()
            .expect("ensured just above");
        Ok(hover::hover_with_index(snapshot, index, &path, offset))
    }

    /// Go to the definition under the cursor. Reads the cached snapshot plus
    /// binding index — never recomputes the project.
    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let position = params.text_document_position_params;
        let url = position.text_document.uri;
        let Some(path) = url_to_path(&url) else {
            return Ok(None);
        };

        let Some((document, documents)) =
            snapshot_document_and_analysis(&self.documents, &url).await
        else {
            return Ok(None);
        };
        let offset = document.index.offset(to_core_position(position.position));
        let Ok(mut analysis) = self.analysis.try_lock() else {
            return Ok(None);
        };
        if self.workspace_invalidation_pending() {
            return Ok(None);
        }
        if fresh_index_if_available(&mut analysis.workspace, &documents, &path, &document.text)
            .is_none()
        {
            return Ok(None);
        }
        let indices = snapshot_indices(&analysis.workspace, &documents);
        let snapshot = analysis.workspace.latest().expect("ensured just above");
        let index = analysis
            .workspace
            .binding_index_cached()
            .expect("ensured just above");
        Ok(
            navigation::definition(snapshot, index, &indices, &path, offset)
                .map(GotoDefinitionResponse::Scalar),
        )
    }

    /// Every reference to the symbol under the cursor, keeping or dropping the
    /// declaration per `includeDeclaration`. Reads the cached binding index.
    async fn references(&self, params: ReferenceParams) -> jsonrpc::Result<Option<Vec<Location>>> {
        let position = params.text_document_position;
        let url = position.text_document.uri;
        let Some(path) = url_to_path(&url) else {
            return Ok(None);
        };
        let include_declaration = params.context.include_declaration;

        let Some((document, documents)) =
            snapshot_document_and_analysis(&self.documents, &url).await
        else {
            return Ok(None);
        };
        let offset = document.index.offset(to_core_position(position.position));
        let Ok(mut analysis) = self.analysis.try_lock() else {
            return Ok(None);
        };
        if self.workspace_invalidation_pending() {
            return Ok(None);
        }
        if fresh_index_if_available(&mut analysis.workspace, &documents, &path, &document.text)
            .is_none()
        {
            return Ok(None);
        }
        let indices = snapshot_indices(&analysis.workspace, &documents);
        let snapshot = analysis.workspace.latest().expect("ensured just above");
        let index = analysis
            .workspace
            .binding_index_cached()
            .expect("ensured just above");
        Ok(navigation::references(
            snapshot,
            index,
            &indices,
            &path,
            offset,
            include_declaration,
        ))
    }

    /// Pre-select the identifier for a rename, or refuse when the cursor is not on a
    /// renameable symbol. Reads the cached binding index.
    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> jsonrpc::Result<Option<PrepareRenameResponse>> {
        let url = params.text_document.uri;
        let Some(path) = url_to_path(&url) else {
            return Ok(None);
        };

        let Some((document, documents)) =
            snapshot_document_and_analysis(&self.documents, &url).await
        else {
            return Ok(None);
        };
        let offset = document.index.offset(to_core_position(params.position));
        let Ok(mut analysis) = self.analysis.try_lock() else {
            return Ok(None);
        };
        if self.workspace_invalidation_pending() {
            return Ok(None);
        }
        if fresh_index_if_available(&mut analysis.workspace, &documents, &path, &document.text)
            .is_none()
        {
            return Ok(None);
        }
        let indices = snapshot_indices(&analysis.workspace, &documents);
        let index = analysis
            .workspace
            .binding_index_cached()
            .expect("ensured just above");
        Ok(navigation::prepare_rename(index, &indices, &path, offset)
            .map(PrepareRenameResponse::Range))
    }

    /// Rename the symbol under the cursor across every reference. Refused with a
    /// JSON-RPC error when the symbol names saved data, so a rename can never
    /// silently orphan stored records. Reads the cached binding index.
    async fn rename(&self, params: RenameParams) -> jsonrpc::Result<Option<WorkspaceEdit>> {
        let position = params.text_document_position;
        let url = position.text_document.uri;
        let Some(path) = url_to_path(&url) else {
            return Ok(None);
        };
        let new_name = params.new_name;

        let Some((document, documents)) =
            snapshot_document_and_analysis(&self.documents, &url).await
        else {
            return Ok(None);
        };
        let offset = document.index.offset(to_core_position(position.position));
        let Ok(mut analysis) = self.analysis.try_lock() else {
            return Ok(None);
        };
        if self.workspace_invalidation_pending() {
            return Ok(None);
        }
        if fresh_index_if_available(&mut analysis.workspace, &documents, &path, &document.text)
            .is_none()
        {
            return Ok(None);
        }
        let indices = snapshot_indices(&analysis.workspace, &documents);
        let index = analysis
            .workspace
            .binding_index_cached()
            .expect("ensured just above");
        match navigation::rename(index, &indices, &path, offset, &new_name) {
            Ok(edit) => Ok(Some(edit)),
            // No symbol under the cursor is not an error — the editor simply does
            // nothing. Known symbols that Marrow cannot rename are refused with a
            // clear message.
            Err(RenameError::NoSymbol) => Ok(None),
            Err(
                error @ (RenameError::NotRenameable
                | RenameError::SavedDataBacked
                | RenameError::InvalidName),
            ) => Err(jsonrpc::Error::invalid_params(error.message())),
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> jsonrpc::Result<Option<DocumentSymbolResponse>> {
        let url = params.text_document.uri;

        // documentSymbol is only sent for an open document, so a missing buffer is
        // a request the client should not have made; outline nothing rather than
        // reach to disk for text the editor already holds.
        let Some(document) = self.documents.lock().await.snapshot(&url) else {
            return Ok(None);
        };
        let outline = symbols::document_symbols(&document.parsed.file, &document.index);
        Ok(Some(DocumentSymbolResponse::Nested(outline)))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> jsonrpc::Result<Option<Vec<SymbolInformation>>> {
        let search_text = params.query;
        let documents = snapshot_documents(&self.documents).await;
        let Ok(analysis) = self.analysis.try_lock() else {
            return Ok(None);
        };
        if self.workspace_invalidation_pending() {
            return Ok(None);
        }
        let Some(snapshot) = analysis.workspace.fresh_latest_from_snapshot(&documents) else {
            return Ok(None);
        };
        Ok(Some(symbols::workspace_symbols(snapshot, &search_text)))
    }

    /// Complete at the cursor. Reads only the document's cached lex/parse and the
    /// fresh checked program — it never re-lexes, re-parses, or
    /// recomputes the project, so it answers instantly and even while the project
    /// has not yet been (re)checked. With no fresh checked facts, an empty program
    /// backs the context-only modes (keywords, builtins).
    async fn completion(
        &self,
        params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
        let position = params.text_document_position;
        let url = position.text_document.uri;
        let Some(path) = url_to_path(&url) else {
            return Ok(None);
        };

        let Some((document, documents)) =
            snapshot_document_and_analysis(&self.documents, &url).await
        else {
            return Ok(None);
        };
        let offset = document.index.offset(to_core_position(position.position));

        let empty = EMPTY_PROGRAM.get_or_init(Default::default);
        let analysis = (!self.workspace_invalidation_pending())
            .then(|| self.analysis.try_lock().ok())
            .flatten();
        let program = analysis
            .as_ref()
            .and_then(|analysis| {
                analysis
                    .workspace
                    .fresh_program_or_empty_from_snapshot(&documents)
            })
            .unwrap_or(empty);

        let items = completion::completion(
            program,
            &path,
            &document.text,
            &document.parsed,
            &document.lexed,
            offset,
        );
        Ok(Some(CompletionResponse::Array(items)))
    }

    /// Show the single callable signature for the innermost call at the cursor.
    /// Reads the open document's cached lex/source and a fresh checked program for
    /// callable shape. No recompute.
    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> jsonrpc::Result<Option<SignatureHelp>> {
        let position = params.text_document_position_params;
        let url = position.text_document.uri;
        let Some(path) = url_to_path(&url) else {
            return Ok(None);
        };

        let Some((document, documents)) =
            snapshot_document_and_analysis(&self.documents, &url).await
        else {
            return Ok(None);
        };
        let offset = document.index.offset(to_core_position(position.position));

        let Ok(analysis) = self.analysis.try_lock() else {
            return Ok(None);
        };
        if self.workspace_invalidation_pending() {
            return Ok(None);
        }
        let Some(snapshot) = analysis.workspace.fresh_latest_from_snapshot(&documents) else {
            return Ok(None);
        };
        let program = &snapshot.program;
        if program.modules.is_empty() {
            return Ok(None);
        }
        Ok(signature_help::signature_help(
            program,
            Some(snapshot),
            &path,
            &document.text,
            &document.lexed,
            offset,
        ))
    }

    /// Classify the document's cached lex into semantic tokens. Reads only the
    /// cached lex, parse, position index, and cached analysis facts — no project
    /// recompute.
    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> jsonrpc::Result<Option<SemanticTokensResult>> {
        let url = params.text_document.uri;
        let Some(path) = url_to_path(&url) else {
            return Ok(None);
        };
        let Some((document, documents)) =
            snapshot_document_and_analysis(&self.documents, &url).await
        else {
            return Ok(None);
        };
        let mut data =
            semantic_tokens::semantic_tokens(&document.lexed, &document.parsed, &document.index);
        if !self.workspace_invalidation_pending()
            && let Ok(mut analysis) = self.analysis.try_lock()
        {
            let has_fresh_analysis =
                snapshot_has_document_text(analysis.workspace.latest(), &path, &document.text)
                    && analysis.workspace.latest_matches_snapshot(&documents);
            if has_fresh_analysis {
                let _ = analysis.workspace.binding_index();
            }
            if let (Some(snapshot), Some(binding_index)) = (
                has_fresh_analysis
                    .then(|| analysis.workspace.latest())
                    .flatten(),
                has_fresh_analysis
                    .then(|| analysis.workspace.binding_index_cached())
                    .flatten(),
            ) && let Some(semantic) = semantic_tokens::semantic_tokens_for_file(
                snapshot,
                binding_index,
                path.as_path(),
                &document.index,
            ) {
                data = semantic;
            }
        }
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }

    /// Format the whole document. Reads only the cached parse and text, calling the
    /// canonical formatter in-process; returns no edits when the buffer has a parse
    /// error, so malformed source is never lossily rewritten. No recompute.
    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        let url = params.text_document.uri;
        let Some(document) = self.documents.lock().await.snapshot(&url) else {
            return Ok(None);
        };
        Ok(formatting::formatting(
            &document.text,
            &document.parsed,
            &document.index,
        ))
    }

    /// Format the indentation of the current line after a newline. Reads only the
    /// open document's cached lex/source and never recomputes the project.
    async fn on_type_formatting(
        &self,
        params: DocumentOnTypeFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        if params.ch != "\n" {
            return Ok(None);
        }
        let url = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let Some(document) = self.documents.lock().await.snapshot(&url) else {
            return Ok(None);
        };
        Ok(indentation::newline_indentation(
            &document.text,
            &document.lexed,
            position,
            &params.options,
        ))
    }
}

/// An empty checked program backing completion before the first analysis lands,
/// so a request can borrow a `&CheckedProgram` without recomputing. Initialized
/// once and shared.
static EMPTY_PROGRAM: std::sync::OnceLock<marrow_lsp_core::workspace::CheckedProgram> =
    std::sync::OnceLock::new();

fn to_core_position(position: Position) -> CorePosition {
    CorePosition {
        line: position.line,
        character: position.character,
    }
}

/// Accept an existing parse-clean snapshot, then build and cache its binding
/// index only when the target file and every analyzed source still matches the
/// editor-visible source world.
/// Returns `None` for stale text or for a file outside any project.
fn fresh_index_if_available<'a>(
    workspace: &'a mut Workspace,
    documents: &DocumentAnalysisSnapshot,
    file: &std::path::Path,
    text: &str,
) -> Option<&'a marrow_lsp_core::workspace::BindingIndex> {
    let snapshot = workspace.latest()?;
    if !snapshot_has_document_text(Some(snapshot), file, text) {
        return None;
    }
    if snapshot_has_parse_errors(snapshot) {
        return None;
    }
    if !workspace.latest_matches_snapshot(documents) {
        return None;
    }
    workspace.binding_index()
}

fn snapshot_has_parse_errors(snapshot: &AnalysisSnapshot) -> bool {
    snapshot.files.iter().any(|file| file.parsed.has_errors())
}

struct WatchedPathChange {
    uri: Url,
    path: PathBuf,
    typ: FileChangeType,
}

enum WatchedProjectChange {
    ProjectConfig {
        root: PathBuf,
        path: PathBuf,
        deleted: bool,
    },
    AnalysisFile {
        root: PathBuf,
        path: PathBuf,
        deleted_url: Option<Url>,
    },
}

#[derive(Default)]
struct PendingWatchedRoot {
    recompute_path: Option<PathBuf>,
    invalidate_project: bool,
    invalidate_analysis: bool,
    clear_root: bool,
    deleted_urls: HashSet<Url>,
}

struct ScheduledRecompute {
    path: PathBuf,
    root_epoch: Option<(PathBuf, u64)>,
}

#[derive(Default)]
struct WatchedRootEffects {
    bits: u8,
    schedules: Vec<ScheduledRecompute>,
    immediate_clears: Vec<PublishedDiagnostics>,
    invalidated_project: bool,
    invalidated_analysis: bool,
}

enum ClearRootDrain {
    Schedule,
    Skip,
}

fn pending_watched_roots(
    events: impl IntoIterator<Item = WatchedProjectChange>,
) -> HashMap<PathBuf, PendingWatchedRoot> {
    let mut pending = HashMap::new();
    for event in events {
        match event {
            WatchedProjectChange::ProjectConfig {
                root,
                path,
                deleted,
            } => {
                let root_pending: &mut PendingWatchedRoot = pending.entry(root).or_default();
                root_pending.invalidate_project = true;
                root_pending.recompute_path = Some(path);
                if deleted {
                    root_pending.clear_root = true;
                    root_pending.deleted_urls.clear();
                }
            }
            WatchedProjectChange::AnalysisFile {
                root,
                path,
                deleted_url,
            } => {
                let root_pending: &mut PendingWatchedRoot = pending.entry(root).or_default();
                root_pending.invalidate_analysis = true;
                if !root_pending.clear_root && root_pending.recompute_path.is_none() {
                    root_pending.recompute_path = Some(path);
                }
                if let Some(url) = deleted_url
                    && !root_pending.clear_root
                {
                    root_pending.deleted_urls.insert(url);
                }
            }
        }
    }
    pending
}

fn watched_root_effects(
    tracker: &mut DiagnosticsTracker,
    versions: &HashMap<Url, Option<i32>>,
    mut pending: HashMap<PathBuf, PendingWatchedRoot>,
    current_root: Option<&Path>,
    clear_root_drain: ClearRootDrain,
) -> WatchedRootEffects {
    let mut effects = WatchedRootEffects::default();
    for (root, pending) in pending.drain() {
        if pending.invalidate_project {
            effects.bits |= PENDING_PROJECT_INVALIDATION;
        }
        if pending.invalidate_analysis {
            effects.bits |= PENDING_ANALYSIS_INVALIDATION;
        }
        if current_root == Some(root.as_path()) {
            effects.invalidated_project |= pending.invalidate_project;
            effects.invalidated_analysis |= pending.invalidate_analysis;
        }
        if pending.clear_root {
            let epoch = tracker.bump_root_epoch(&root);
            effects.immediate_clears.extend(
                tracker
                    .clear_root(&root)
                    .into_iter()
                    .map(|url| clear_publish(url, versions)),
            );
            if matches!(clear_root_drain, ClearRootDrain::Schedule)
                && let Some(path) = pending.recompute_path
            {
                effects.schedules.push(ScheduledRecompute {
                    path,
                    root_epoch: Some((root, epoch)),
                });
            }
            continue;
        }
        for url in sorted_urls(pending.deleted_urls) {
            if versions.contains_key(&url) {
                continue;
            }
            if tracker.clear_url(&root, &url) {
                effects.immediate_clears.push(clear_publish(url, versions));
            }
        }
        if let Some(path) = pending.recompute_path {
            let epoch = tracker.bump_root_epoch(&root);
            effects.schedules.push(ScheduledRecompute {
                path,
                root_epoch: Some((root, epoch)),
            });
        }
    }
    effects
}

fn watched_project_changes(
    workspace: &Workspace,
    changes: &[WatchedPathChange],
) -> Vec<WatchedProjectChange> {
    let Some(project) = workspace.project() else {
        return Vec::new();
    };
    let mut watched = Vec::new();
    let config_path = project.root.join("marrow.json");
    if let Some(change) = changes.iter().find(|change| change.path == config_path) {
        watched.push(WatchedProjectChange::ProjectConfig {
            root: project.root.clone(),
            path: change.path.clone(),
            deleted: change.typ == FileChangeType::DELETED,
        });
        return watched;
    }
    let lock_path = project
        .root
        .join(marrow_lsp_core::workspace::COMMITTED_LOCK_FILE);
    if let Some(change) = changes.iter().find(|change| change.path == lock_path) {
        watched.push(WatchedProjectChange::AnalysisFile {
            root: project.root.clone(),
            path: change.path.clone(),
            deleted_url: None,
        });
    }
    if let Some(change) = changes.iter().find(|change| {
        project.analyzes_file(&change.path) || snapshot_has_file(workspace.latest(), &change.path)
    }) {
        watched.push(WatchedProjectChange::AnalysisFile {
            root: project.root.clone(),
            path: change.path.clone(),
            deleted_url: (change.typ == FileChangeType::DELETED).then(|| change.uri.clone()),
        });
    }
    watched
}

fn watched_tracked_diagnostic_changes(
    tracker: &DiagnosticsTracker,
    changes: &[WatchedPathChange],
) -> Vec<WatchedProjectChange> {
    let mut watched = Vec::new();
    for change in changes {
        if let Some(root) = tracker.tracked_root_for_config_path(&change.path) {
            watched.push(WatchedProjectChange::ProjectConfig {
                root,
                path: change.path.clone(),
                deleted: change.typ == FileChangeType::DELETED,
            });
            continue;
        }
        let Some(root) = tracker.tracked_root_for_path(&change.path) else {
            continue;
        };
        if tracked_root_analysis_change(&root, change) {
            let deleted_url = (change.typ == FileChangeType::DELETED
                && tracker.tracked_root_for_url(&change.uri).as_deref() == Some(root.as_path()))
            .then(|| change.uri.clone());
            watched.push(WatchedProjectChange::AnalysisFile {
                root,
                path: change.path.clone(),
                deleted_url,
            });
        }
    }
    watched
}

fn busy_watched_project_changes(
    tracker: &DiagnosticsTracker,
    changes: &[WatchedPathChange],
) -> Vec<WatchedProjectChange> {
    let mut watched = watched_tracked_diagnostic_changes(tracker, changes);
    watched.extend(watched_untracked_project_changes(tracker, changes));
    watched
}

fn busy_watched_pre_invalidation_bits(changes: &[WatchedPathChange]) -> u8 {
    let mut bits = 0;
    for change in changes {
        if change.path.file_name().and_then(|name| name.to_str()) == Some("marrow.json") {
            bits |= PENDING_PROJECT_INVALIDATION;
        } else if (change.path.file_name().and_then(|name| name.to_str())
            == Some(marrow_lsp_core::workspace::COMMITTED_LOCK_FILE)
            || change.path.extension().and_then(|ext| ext.to_str()) == Some("mw"))
            && nearest_project_root(&change.path).is_some()
        {
            bits |= PENDING_ANALYSIS_INVALIDATION;
        }
    }
    bits
}

fn watched_untracked_project_changes(
    tracker: &DiagnosticsTracker,
    changes: &[WatchedPathChange],
) -> Vec<WatchedProjectChange> {
    let mut watched = Vec::new();
    for change in changes {
        if tracker.tracked_root_for_config_path(&change.path).is_some()
            || tracker.tracked_root_for_path(&change.path).is_some()
        {
            continue;
        }
        if change.path.file_name().and_then(|name| name.to_str()) == Some("marrow.json") {
            if let Some(root) = change.path.parent() {
                watched.push(WatchedProjectChange::ProjectConfig {
                    root: root.to_path_buf(),
                    path: change.path.clone(),
                    deleted: change.typ == FileChangeType::DELETED,
                });
            }
            continue;
        }
        let Some(root) = nearest_project_root(&change.path) else {
            continue;
        };
        if tracked_root_analysis_change(&root, change) {
            watched.push(WatchedProjectChange::AnalysisFile {
                root,
                path: change.path.clone(),
                deleted_url: None,
            });
        }
    }
    watched
}

fn nearest_project_root(path: &Path) -> Option<PathBuf> {
    let mut directory = path.parent();
    while let Some(current) = directory {
        if current.join("marrow.json").is_file() {
            return Some(current.to_path_buf());
        }
        directory = current.parent();
    }
    None
}

fn tracked_root_analysis_change(root: &Path, change: &WatchedPathChange) -> bool {
    change.path == root.join(marrow_lsp_core::workspace::COMMITTED_LOCK_FILE)
        || change.path.extension().and_then(|ext| ext.to_str()) == Some("mw")
}

fn snapshot_has_file(snapshot: Option<&AnalysisSnapshot>, path: &std::path::Path) -> bool {
    snapshot.is_some_and(|snapshot| snapshot.files.iter().any(|file| file.path == path))
}

/// Build the per-file index resolver navigation maps spans through: one
/// [`LineIndex`](marrow_lsp_core::positions::LineIndex) per file the snapshot
/// analyzed, the open buffer's when held (so spans land on unsaved text), else one
/// built from disk. Called once per navigation request over the bounded project
/// file set.
fn snapshot_indices<'a>(
    workspace: &'a Workspace,
    documents: &'a DocumentAnalysisSnapshot,
) -> SnapshotIndices<'a> {
    let files = workspace
        .latest()
        .map(|snapshot| snapshot.files.iter().map(|file| file.path.as_path()))
        .into_iter()
        .flatten();
    SnapshotIndices::new(files, |path| documents.line_index_for_path(path))
}

fn snapshot_has_document_text(
    snapshot: Option<&AnalysisSnapshot>,
    file: &std::path::Path,
    text: &str,
) -> bool {
    snapshot
        .and_then(|snapshot| snapshot.files.iter().find(|analyzed| analyzed.path == file))
        .is_some_and(|analyzed| analyzed.source == text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_source_match_rejects_freshly_edited_document_text() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("m.mw");
        let clean = "module m\n\nfn f(): int\n    return 1\n";
        std::fs::write(&file, clean).unwrap();

        let url = Url::from_file_path(&file).unwrap();
        let mut documents = Documents::new();
        documents.open_with_version(url.clone(), clean.to_string(), None);
        let mut workspace = Workspace::new();
        workspace.recompute(&file, &documents).unwrap();

        assert!(snapshot_has_document_text(workspace.latest(), &file, clean));

        let edited = "module m\n\nfn f(): int\n    return 2\n";
        documents.change_with_version(&url, edited.to_string(), None);
        let document = documents.get(&url).unwrap();

        assert!(
            !snapshot_has_document_text(workspace.latest(), &file, &document.text),
            "semantic tokens should not apply binding facts to text newer than the snapshot"
        );
    }

    #[test]
    fn snapshot_source_match_rejects_a_stale_open_sibling_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let a = src.join("a.mw");
        let b = src.join("b.mw");
        let a_source = "module a\n\nfn f(): int\n    return 1\n";
        let b_source = "module b\n\nfn g(): int\n    return 2\n";
        std::fs::write(&a, a_source).unwrap();
        std::fs::write(&b, b_source).unwrap();

        let a_url = Url::from_file_path(&a).unwrap();
        let b_url = Url::from_file_path(&b).unwrap();
        let mut documents = Documents::new();
        documents.open_with_version(a_url, a_source.to_string(), None);
        documents.open_with_version(b_url.clone(), b_source.to_string(), None);
        let mut workspace = Workspace::new();
        workspace.recompute(&a, &documents).unwrap();

        assert!(workspace.latest_matches_sources(&documents));

        documents.change_with_version(
            &b_url,
            "module b\n\nfn g(): int\n    return 3\n".to_string(),
            None,
        );

        assert!(
            !workspace.latest_matches_sources(&documents),
            "a project-wide binding index must not be exposed when another open snapshot file is stale"
        );
    }

    #[test]
    fn snapshot_source_match_rejects_a_new_open_source_file_absent_from_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["a", "z"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let a_src = root.join("a");
        let z_src = root.join("z");
        std::fs::create_dir_all(&a_src).unwrap();
        std::fs::create_dir_all(&z_src).unwrap();
        let app = z_src.join("app.mw");
        let app_source = "module app\n\nfn f(): int\n    return 1\n";
        std::fs::write(&app, app_source).unwrap();

        let app_url = Url::from_file_path(&app).unwrap();
        let mut documents = Documents::new();
        documents.open_with_version(app_url, app_source.to_string(), None);
        let mut workspace = Workspace::new();
        workspace.recompute(&app, &documents).unwrap();

        assert!(workspace.latest_matches_sources(&documents));

        let new_defs = a_src.join("defs.mw");
        let defs_url = Url::from_file_path(&new_defs).unwrap();
        documents.open_with_version(
            defs_url,
            "module defs\n\npub fn answer(): int\n    return 1\n".to_string(),
            None,
        );

        assert!(
            !workspace.latest_matches_sources(&documents),
            "a new open source-root file absent from the cached snapshot can change project bindings"
        );
    }

    #[test]
    fn request_index_does_not_recompute_when_analysis_is_missing() {
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
        let source = "module app\n\nfn f(): int\n    return 1\n";
        std::fs::write(&file, source).unwrap();

        let url = Url::from_file_path(&file).unwrap();
        let mut documents = Documents::new();
        documents.open_with_version(url, source.to_string(), Some(1));
        let mut workspace = Workspace::new();

        assert!(
            fresh_index_if_available(
                &mut workspace,
                &documents.analysis_snapshot(),
                &file,
                source
            )
            .is_none(),
            "request paths must not synchronously compute missing analysis"
        );
        assert!(
            workspace.latest().is_none(),
            "missing analysis should remain missing until a scheduled recompute lands"
        );
    }

    #[tokio::test]
    async fn live_document_access_does_not_wait_for_analysis_state_lock() {
        let url = Url::parse("file:///tmp/project/src/app.mw").unwrap();
        let mut documents = Documents::new();
        documents.open_with_version(url.clone(), "module app\n".to_string(), Some(1));
        let documents = Arc::new(Mutex::new(documents));
        let analysis = Arc::new(Mutex::new(AnalysisState {
            workspace: Workspace::new(),
            recompute_gate: None,
        }));

        let analysis_guard = analysis.lock().await;
        let documents_for_request = Arc::clone(&documents);
        let result = tokio::time::timeout(std::time::Duration::from_millis(20), async move {
            documents_for_request
                .lock()
                .await
                .snapshot(&url)
                .map(|document| document.version)
        })
        .await;
        drop(analysis_guard);

        assert!(
            result.is_ok(),
            "document-only request state must not wait for project analysis"
        );
        assert_eq!(result.unwrap(), Some(Some(1)));
    }

    #[tokio::test]
    async fn live_document_notifications_and_outline_do_not_wait_for_recompute() {
        let (service, _socket) = tower_lsp::LspService::build(Backend::new).finish();
        let backend = service.inner();
        let gate = TestRecomputeGate::new();
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
        std::fs::write(&file, "module app\n").unwrap();
        let url = Url::from_file_path(&file).unwrap();
        {
            let mut documents = backend.documents.lock().await;
            documents.open_with_version(url.clone(), "module app\n".to_string(), Some(1));
        }
        {
            let mut diagnostics = backend.diagnostics.lock().await;
            diagnostics.bump_root_epoch(root);
        }
        {
            let mut analysis = backend.analysis.lock().await;
            analysis.recompute_gate = Some(Arc::clone(&gate));
        }

        let documents = Arc::clone(&backend.documents);
        let analysis = Arc::clone(&backend.analysis);
        let diagnostics = Arc::clone(&backend.diagnostics);
        let diagnostics_publish = Arc::clone(&backend.diagnostics_publish);
        let pending_workspace_invalidation = Arc::clone(&backend.pending_workspace_invalidation);
        let client = backend.client.clone();
        let recompute = tokio::spawn(async move {
            Backend::recompute_and_publish(
                RecomputeContext {
                    documents: &documents,
                    analysis: &analysis,
                    diagnostics: &diagnostics,
                    diagnostics_publish: &diagnostics_publish,
                    pending_workspace_invalidation: &pending_workspace_invalidation,
                    client: &client,
                },
                &file,
                None,
            )
            .await;
        });
        gate.wait_until_entered().await;

        let changed = tokio::time::timeout(std::time::Duration::from_millis(50), async {
            <Backend as LanguageServer>::did_change(
                backend,
                DidChangeTextDocumentParams {
                    text_document: VersionedTextDocumentIdentifier {
                        uri: url.clone(),
                        version: 2,
                    },
                    content_changes: vec![TextDocumentContentChangeEvent {
                        range: None,
                        range_length: None,
                        text: "module app\n\nfn f()\n    return\n".to_string(),
                    }],
                },
            )
            .await;
        })
        .await;
        assert!(
            changed.is_ok(),
            "didChange must update live documents without waiting for project recompute"
        );

        let symbols = tokio::time::timeout(std::time::Duration::from_millis(50), async {
            <Backend as LanguageServer>::document_symbol(
                backend,
                DocumentSymbolParams {
                    text_document: TextDocumentIdentifier { uri: url.clone() },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                },
            )
            .await
            .unwrap()
        })
        .await
        .expect("document symbols must not wait for project recompute");
        assert!(
            symbols.is_some(),
            "document symbols should use the live document snapshot"
        );

        let unrelated_url = Url::from_file_path(dir.path().join("scratch.mw")).unwrap();
        let watched = tokio::time::timeout(std::time::Duration::from_millis(50), async {
            <Backend as LanguageServer>::did_change_watched_files(
                backend,
                DidChangeWatchedFilesParams {
                    changes: vec![
                        FileEvent {
                            uri: unrelated_url,
                            typ: FileChangeType::CHANGED,
                        },
                        FileEvent {
                            uri: url.clone(),
                            typ: FileChangeType::CHANGED,
                        },
                    ],
                },
            )
            .await;
        })
        .await;
        assert!(
            watched.is_ok(),
            "watched-file changes must not wait for project recompute"
        );
        assert_ne!(
            backend
                .pending_workspace_invalidation
                .load(Ordering::SeqCst)
                & PENDING_ANALYSIS_INVALIDATION,
            0,
            "busy watched-file changes under a tracked root must invalidate pending analysis before publish"
        );

        let hover = tokio::time::timeout(std::time::Duration::from_millis(50), async {
            <Backend as LanguageServer>::hover(
                backend,
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier { uri: url.clone() },
                        position: Position {
                            line: 2,
                            character: 3,
                        },
                    },
                    work_done_progress_params: WorkDoneProgressParams::default(),
                },
            )
            .await
            .unwrap()
        })
        .await
        .expect("hover must not wait for project recompute");
        assert!(
            hover.is_none(),
            "analysis-backed hover should return null while fresh analysis is unavailable"
        );

        let closed = tokio::time::timeout(std::time::Duration::from_millis(50), async {
            <Backend as LanguageServer>::did_close(
                backend,
                DidCloseTextDocumentParams {
                    text_document: TextDocumentIdentifier { uri: url.clone() },
                },
            )
            .await;
        })
        .await;
        assert!(
            closed.is_ok(),
            "didClose must drop live documents without waiting for project recompute"
        );
        assert!(backend.documents.lock().await.snapshot(&url).is_none());

        gate.release().await;
        recompute.await.unwrap();
    }

    #[tokio::test]
    async fn busy_watcher_config_delete_clear_waits_for_publish_order() {
        let (service, _socket) = tower_lsp::LspService::build(Backend::new).finish();
        let backend = service.inner();
        let root = std::path::PathBuf::from("/tmp/project-a");
        let config = root.join("marrow.json");
        let bad = root.join("src/bad.mw");
        let config_url = Url::from_file_path(&config).unwrap();
        let bad_url = Url::from_file_path(&bad).unwrap();
        {
            let mut diagnostics = backend.diagnostics.lock().await;
            let epoch = diagnostics.bump_root_epoch(&root);
            let mut published = std::collections::HashMap::from([(bad_url.clone(), non_empty())]);
            assert!(diagnostics.reconcile_success(&root, epoch, &mut published));
        }

        let analysis_guard = backend.analysis.lock().await;
        let publish_guard = backend.diagnostics_publish.lock().await;
        let clear = <Backend as LanguageServer>::did_change_watched_files(
            backend,
            DidChangeWatchedFilesParams {
                changes: vec![FileEvent {
                    uri: config_url.clone(),
                    typ: FileChangeType::DELETED,
                }],
            },
        );
        tokio::pin!(clear);

        let blocked = tokio::time::timeout(std::time::Duration::from_millis(20), &mut clear).await;
        assert!(
            blocked.is_err(),
            "config-delete clears must wait for diagnostic publish ordering"
        );
        assert_eq!(
            backend
                .pending_workspace_invalidation
                .load(Ordering::SeqCst)
                & PENDING_PROJECT_INVALIDATION,
            0,
            "watcher application waits behind the active diagnostic publish"
        );
        assert!(backend.diagnostics.lock().await.is_tracked(&root, &bad_url));

        drop(publish_guard);
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut clear)
            .await
            .expect("clear should publish after the publish ordering lock is released");
        assert_ne!(
            backend
                .pending_workspace_invalidation
                .load(Ordering::SeqCst)
                & PENDING_PROJECT_INVALIDATION,
            0
        );
        assert!(!backend.diagnostics.lock().await.is_tracked(&root, &bad_url));
        drop(analysis_guard);
        <Backend as LanguageServer>::did_change_watched_files(
            backend,
            DidChangeWatchedFilesParams {
                changes: vec![FileEvent {
                    uri: config_url,
                    typ: FileChangeType::DELETED,
                }],
            },
        )
        .await;
        assert_eq!(
            backend
                .pending_workspace_invalidation
                .load(Ordering::SeqCst)
                & PENDING_PROJECT_INVALIDATION,
            0,
            "duplicate normal config-delete events must drain pending invalidation before they can stale the busy drain epoch"
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if backend
                    .pending_workspace_invalidation
                    .load(Ordering::SeqCst)
                    & PENDING_PROJECT_INVALIDATION
                    == 0
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("busy config delete should schedule a recompute to drain pending invalidation");
    }

    #[tokio::test]
    async fn busy_watcher_sets_pending_before_waiting_on_diagnostics() {
        let (service, _socket) = tower_lsp::LspService::build(Backend::new).finish();
        let backend = service.inner();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let changed = src.join("bad.mw");
        let changed_url = Url::from_file_path(&changed).unwrap();

        let analysis_guard = backend.analysis.lock().await;
        let diagnostics_guard = backend.diagnostics.lock().await;
        let watched = <Backend as LanguageServer>::did_change_watched_files(
            backend,
            DidChangeWatchedFilesParams {
                changes: vec![FileEvent {
                    uri: changed_url,
                    typ: FileChangeType::CHANGED,
                }],
            },
        );
        tokio::pin!(watched);

        let blocked =
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut watched).await;
        assert!(
            blocked.is_err(),
            "test setup should leave the busy watcher queued on diagnostics"
        );
        assert_ne!(
            backend
                .pending_workspace_invalidation
                .load(Ordering::SeqCst)
                & PENDING_ANALYSIS_INVALIDATION,
            0,
            "busy watcher must mark project-file invalidation before waiting on diagnostics"
        );

        drop(diagnostics_guard);
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut watched)
            .await
            .expect("watcher should finish once diagnostics lock is released");
        drop(analysis_guard);
    }

    #[tokio::test]
    async fn busy_watcher_holds_publish_before_first_await() {
        let (service, _socket) = tower_lsp::LspService::build(Backend::new).finish();
        let backend = service.inner();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let changed = src.join("bad.mw");
        let changed_url = Url::from_file_path(&changed).unwrap();

        let analysis_guard = backend.analysis.lock().await;
        let documents_guard = backend.documents.lock().await;
        let watched = <Backend as LanguageServer>::did_change_watched_files(
            backend,
            DidChangeWatchedFilesParams {
                changes: vec![FileEvent {
                    uri: changed_url,
                    typ: FileChangeType::CHANGED,
                }],
            },
        );
        tokio::pin!(watched);

        let blocked =
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut watched).await;
        assert!(
            blocked.is_err(),
            "test setup should leave the watcher queued on the document snapshot"
        );
        assert!(
            backend.diagnostics_publish.try_lock().is_err(),
            "watcher must hold diagnostic publish ordering before any await can deschedule it"
        );

        drop(documents_guard);
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut watched)
            .await
            .expect("watcher should finish once the document lock is released");
        assert_ne!(
            backend
                .pending_workspace_invalidation
                .load(Ordering::SeqCst)
                & PENDING_ANALYSIS_INVALIDATION,
            0,
            "busy watcher must mark project-file invalidation before releasing publish ordering"
        );
        drop(analysis_guard);
    }

    #[tokio::test]
    async fn saved_data_requests_do_not_wait_for_recompute() {
        let (service, _socket) = tower_lsp::LspService::build(Backend::new).finish();
        let backend = service.inner();
        let analysis_guard = backend.analysis.lock().await;

        let roots =
            tokio::time::timeout(std::time::Duration::from_millis(50), backend.saved_roots()).await;
        assert!(
            roots.is_ok(),
            "savedRoots must return unavailable instead of waiting for analysis"
        );
        assert!(!roots.unwrap().unwrap().available);

        let children = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            backend.data_children(DataChildrenRequest {
                segments: vec![marrow_lsp_core::data_explorer::DataPathSegmentDto::Root {
                    store_catalog_id: "cat_00000000000000000000000000000001".into(),
                }],
                limit: 1,
                cursor: None,
            }),
        )
        .await;
        assert!(
            children.is_ok(),
            "dataChildren must return unavailable instead of waiting for analysis"
        );
        assert!(!children.unwrap().unwrap().available);

        let read = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            backend.data_read(DataReadRequest {
                segments: vec![marrow_lsp_core::data_explorer::DataPathSegmentDto::Root {
                    store_catalog_id: "cat_00000000000000000000000000000001".into(),
                }],
                preview_limit: None,
            }),
        )
        .await;
        assert!(
            read.is_ok(),
            "dataRead must return unavailable instead of waiting for analysis"
        );
        assert!(!read.unwrap().unwrap().available);

        let watch_targets = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            backend.data_watch_targets(),
        )
        .await;
        assert!(
            watch_targets.is_ok(),
            "dataWatchTargets must return empty instead of waiting for analysis"
        );
        assert!(watch_targets.unwrap().unwrap().paths.is_empty());

        drop(analysis_guard);
    }

    #[test]
    fn open_analysis_document_urls_exclude_other_projects_and_non_analysis_files() {
        let project_a = tempfile::tempdir().unwrap();
        std::fs::write(
            project_a.path().join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src_a = project_a.path().join("src");
        std::fs::create_dir_all(&src_a).unwrap();
        let app_a = src_a.join("app.mw");
        std::fs::write(&app_a, "module app\n\npub fn broken\n").unwrap();

        let project_b = tempfile::tempdir().unwrap();
        std::fs::write(
            project_b.path().join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src_b = project_b.path().join("src");
        std::fs::create_dir_all(&src_b).unwrap();
        let app_b = src_b.join("app.mw");
        std::fs::write(&app_b, "module app\n\npub fn main()\n    return\n").unwrap();

        let notes_b = project_b.path().join("notes.txt");
        let url_a = Url::from_file_path(&app_a).unwrap();
        let url_b = Url::from_file_path(&app_b).unwrap();
        let notes_url = Url::from_file_path(&notes_b).unwrap();
        let mut documents = Documents::new();
        documents.open_with_version(url_a, "module app\n\npub fn broken\n".to_string(), Some(1));
        documents.open_with_version(
            url_b.clone(),
            "module app\n\npub fn main()\n    return\n".to_string(),
            Some(1),
        );
        documents.open_with_version(notes_url, "not marrow".to_string(), Some(1));

        let mut workspace = Workspace::new();
        workspace.recompute(&app_b, &documents).unwrap();

        assert_eq!(
            open_analysis_document_urls(
                &documents.analysis_snapshot(),
                workspace.project().unwrap()
            ),
            vec![url_b],
            "diagnostic publishes for project B must not include open documents from other projects"
        );
    }

    #[test]
    fn clear_publish_uses_open_document_version() {
        let url = Url::parse("file:///tmp/project-a/src/app.mw").unwrap();
        let versions = std::collections::HashMap::from([(url.clone(), Some(7))]);

        let publish = clear_publish(url.clone(), &versions);

        assert_eq!(publish.url, url);
        assert!(publish.diagnostics.is_empty());
        assert_eq!(publish.version, Some(7));
    }

    #[test]
    fn watched_project_change_invalidates_analysis_for_committed_lock() {
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
        std::fs::write(&file, "module app\n\nfn f(): int\n    return 1\n").unwrap();

        let documents = Documents::new();
        let mut workspace = Workspace::new();
        workspace.recompute(&file, &documents).unwrap();

        let lock_path = root.join(marrow_lsp_core::workspace::COMMITTED_LOCK_FILE);
        let lock_uri = Url::from_file_path(&lock_path).unwrap();
        let changed = [WatchedPathChange {
            uri: lock_uri,
            path: lock_path.clone(),
            typ: FileChangeType::CHANGED,
        }];
        let mut changes = watched_project_changes(&workspace, &changed);
        assert_eq!(changes.len(), 1);
        let change = changes.pop();

        match change {
            Some(WatchedProjectChange::AnalysisFile { path, .. }) => assert_eq!(path, lock_path),
            Some(WatchedProjectChange::ProjectConfig { path, .. }) => {
                panic!("committed lock should not invalidate project config: {path:?}")
            }
            None => panic!("committed lock should invalidate analysis"),
        }
    }

    #[test]
    fn batched_watcher_changes_keep_current_and_tracked_roots() {
        let project_a = tempfile::tempdir().unwrap();
        let root_a = project_a.path().to_path_buf();
        let src_a = root_a.join("src");
        std::fs::create_dir_all(&src_a).unwrap();
        let bad_a = src_a.join("bad.mw");
        let bad_a_url = Url::from_file_path(&bad_a).unwrap();
        let mut tracker = DiagnosticsTracker::new();
        let epoch_a = tracker.bump_root_epoch(&root_a);
        let mut diagnostics = std::collections::HashMap::from([(bad_a_url.clone(), non_empty())]);
        assert!(tracker.reconcile_success(&root_a, epoch_a, &mut diagnostics));

        let project_b = tempfile::tempdir().unwrap();
        let root_b = project_b.path().to_path_buf();
        std::fs::write(
            root_b.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src_b = root_b.join("src");
        std::fs::create_dir_all(&src_b).unwrap();
        let app_b = src_b.join("app.mw");
        std::fs::write(&app_b, "module app\n\npub fn main()\n    return\n").unwrap();
        let app_b_url = Url::from_file_path(&app_b).unwrap();
        let mut workspace = Workspace::new();
        workspace.recompute(&app_b, &Documents::new()).unwrap();

        let changes = [
            WatchedPathChange {
                uri: app_b_url,
                path: app_b,
                typ: FileChangeType::CHANGED,
            },
            WatchedPathChange {
                uri: bad_a_url,
                path: bad_a,
                typ: FileChangeType::CHANGED,
            },
        ];
        let pending = pending_watched_roots(
            watched_project_changes(&workspace, &changes)
                .into_iter()
                .chain(watched_tracked_diagnostic_changes(&tracker, &changes)),
        );

        assert!(
            pending.contains_key(&root_a),
            "tracked project A must still be processed when project B is also in the watcher batch"
        );
        assert!(
            pending.contains_key(&root_b),
            "the current project B change should remain scheduled"
        );
    }

    #[test]
    fn busy_watcher_recompute_uses_tracked_path_from_mixed_batch() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        let src = root.join("src");
        let tracked = src.join("app.mw");
        let unrelated = dir.path().join("scratch.mw");
        let tracked_url = Url::from_file_path(&tracked).unwrap();
        let unrelated_url = Url::from_file_path(&unrelated).unwrap();
        let mut tracker = DiagnosticsTracker::new();
        tracker.bump_root_epoch(&root);

        let changes = [
            WatchedPathChange {
                uri: unrelated_url,
                path: unrelated,
                typ: FileChangeType::CHANGED,
            },
            WatchedPathChange {
                uri: tracked_url,
                path: tracked.clone(),
                typ: FileChangeType::CHANGED,
            },
        ];
        let pending = pending_watched_roots(watched_tracked_diagnostic_changes(&tracker, &changes));
        let effects = watched_root_effects(
            &mut tracker,
            &std::collections::HashMap::new(),
            pending,
            None,
            ClearRootDrain::Schedule,
        );

        assert_ne!(
            effects.bits & PENDING_ANALYSIS_INVALIDATION,
            0,
            "tracked analysis changes should mark pending analysis stale"
        );
        assert_eq!(effects.schedules.len(), 1);
        assert_eq!(
            effects.schedules[0].path, tracked,
            "busy watcher recompute must use the tracked change path, not the first raw batch event"
        );
        assert_eq!(
            effects.schedules[0]
                .root_epoch
                .as_ref()
                .map(|(scheduled_root, _)| scheduled_root.as_path()),
            Some(root.as_path())
        );
    }

    #[test]
    fn busy_watcher_discovers_untracked_project_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let changed = root.join("src/bad.mw");
        let changed_url = Url::from_file_path(&changed).unwrap();
        let scratch = dir.path().join("scratch.mw");
        let scratch_url = Url::from_file_path(&scratch).unwrap();
        let tracker = DiagnosticsTracker::new();

        let changes = [
            WatchedPathChange {
                uri: scratch_url,
                path: scratch,
                typ: FileChangeType::CHANGED,
            },
            WatchedPathChange {
                uri: changed_url,
                path: changed.clone(),
                typ: FileChangeType::CHANGED,
            },
        ];
        let pending = pending_watched_roots(busy_watched_project_changes(&tracker, &changes));
        assert!(
            !pending.contains_key(dir.path()),
            "loose scratch files outside a project should not create busy watcher work"
        );
        let mut tracker = tracker;
        let effects = watched_root_effects(
            &mut tracker,
            &std::collections::HashMap::new(),
            pending,
            None,
            ClearRootDrain::Schedule,
        );

        assert_ne!(
            effects.bits & PENDING_ANALYSIS_INVALIDATION,
            0,
            "untracked project changes should still invalidate an in-flight first recompute"
        );
        assert_eq!(effects.schedules.len(), 1);
        assert_eq!(effects.schedules[0].path, changed);
        assert_eq!(
            effects.schedules[0]
                .root_epoch
                .as_ref()
                .map(|(scheduled_root, _)| scheduled_root.as_path()),
            Some(root.as_path())
        );
    }

    #[test]
    fn busy_watcher_config_delete_clears_tracked_root_diagnostics() {
        let root = std::path::PathBuf::from("/tmp/project-a");
        let config = root.join("marrow.json");
        let bad = root.join("src/bad.mw");
        let config_url = Url::from_file_path(&config).unwrap();
        let bad_url = Url::from_file_path(&bad).unwrap();
        let mut tracker = DiagnosticsTracker::new();
        let epoch = tracker.bump_root_epoch(&root);
        let mut diagnostics = std::collections::HashMap::from([(bad_url.clone(), non_empty())]);
        assert!(tracker.reconcile_success(&root, epoch, &mut diagnostics));

        let changes = [WatchedPathChange {
            uri: config_url,
            path: config.clone(),
            typ: FileChangeType::DELETED,
        }];
        let pending = pending_watched_roots(watched_tracked_diagnostic_changes(&tracker, &changes));
        let effects = watched_root_effects(
            &mut tracker,
            &std::collections::HashMap::new(),
            pending,
            None,
            ClearRootDrain::Schedule,
        );

        assert_ne!(
            effects.bits & PENDING_PROJECT_INVALIDATION,
            0,
            "tracked config deletes should mark pending project state stale"
        );
        assert_eq!(effects.schedules.len(), 1);
        assert_eq!(effects.schedules[0].path, config);
        assert_eq!(
            effects.schedules[0]
                .root_epoch
                .as_ref()
                .map(|(scheduled_root, _)| scheduled_root.as_path()),
            Some(root.as_path())
        );
        assert_eq!(effects.immediate_clears.len(), 1);
        assert_eq!(effects.immediate_clears[0].url, bad_url);
        assert!(effects.immediate_clears[0].diagnostics.is_empty());
        assert!(
            !tracker.is_tracked(&root, &bad_url),
            "cleared root diagnostics should be removed from the tracker before publishing empties"
        );
    }

    #[test]
    fn tracked_root_watcher_changes_are_root_based() {
        let root = std::path::PathBuf::from("/tmp/project-a");
        let config = root.join("marrow.json");
        let sibling = root.join("src/defs.mw");
        let config_url = Url::from_file_path(&config).unwrap();
        let sibling_url = Url::from_file_path(&sibling).unwrap();
        let mut tracker = DiagnosticsTracker::new();
        tracker.bump_root_epoch(&root);

        let changes = [
            WatchedPathChange {
                uri: config_url,
                path: config.clone(),
                typ: FileChangeType::CHANGED,
            },
            WatchedPathChange {
                uri: sibling_url,
                path: sibling.clone(),
                typ: FileChangeType::CHANGED,
            },
        ];
        let watched = watched_tracked_diagnostic_changes(&tracker, &changes);

        assert!(
            watched.iter().any(|change| matches!(
                change,
                WatchedProjectChange::ProjectConfig {
                    root: event_root,
                    path,
                    deleted: false,
                } if event_root == &root && path == &config
            )),
            "tracked config changes should be routed even when the root is not current"
        );
        assert!(
            watched.iter().any(|change| matches!(
                change,
                WatchedProjectChange::AnalysisFile {
                    root: event_root,
                    path,
                    deleted_url: None,
                } if event_root == &root && path == &sibling
            )),
            "tracked-root .mw changes should schedule recompute even when that URL had no diagnostics"
        );
    }

    #[test]
    fn diagnostics_tracker_omitted_tracked_url_returns_clear_for_same_root() {
        let root = std::path::PathBuf::from("/tmp/project-a");
        let previous = Url::parse("file:///tmp/project-a/src/bad.mw").unwrap();
        let current = Url::parse("file:///tmp/project-a/src/app.mw").unwrap();
        let mut tracker = DiagnosticsTracker::new();

        let epoch = tracker.bump_root_epoch(&root);
        let mut first = std::collections::HashMap::from([(previous.clone(), non_empty())]);
        assert!(tracker.reconcile_success(&root, epoch, &mut first));

        let epoch = tracker.bump_root_epoch(&root);
        let mut next = std::collections::HashMap::from([(current.clone(), non_empty())]);
        assert!(tracker.reconcile_success(&root, epoch, &mut next));

        assert!(
            next.get(&previous).is_some_and(Vec::is_empty),
            "same-root success should clear a previously non-empty URL that disappeared"
        );
        assert!(!tracker.is_tracked(&root, &previous));
        assert!(tracker.is_tracked(&root, &current));
    }

    #[test]
    fn diagnostics_tracker_root_b_result_does_not_clear_root_a() {
        let root_a = std::path::PathBuf::from("/tmp/project-a");
        let root_b = std::path::PathBuf::from("/tmp/project-b");
        let url_a = Url::parse("file:///tmp/project-a/src/bad.mw").unwrap();
        let url_b = Url::parse("file:///tmp/project-b/src/app.mw").unwrap();
        let mut tracker = DiagnosticsTracker::new();

        let epoch_a = tracker.bump_root_epoch(&root_a);
        let mut first = std::collections::HashMap::from([(url_a.clone(), non_empty())]);
        assert!(tracker.reconcile_success(&root_a, epoch_a, &mut first));

        let epoch_b = tracker.bump_root_epoch(&root_b);
        let mut second = std::collections::HashMap::from([(url_b.clone(), non_empty())]);
        assert!(tracker.reconcile_success(&root_b, epoch_b, &mut second));

        assert!(
            !second.contains_key(&url_a),
            "root B success must not produce clears for root A"
        );
        assert!(tracker.is_tracked(&root_a, &url_a));
        assert!(tracker.is_tracked(&root_b, &url_b));
    }

    #[test]
    fn diagnostics_tracker_paths_outside_tracked_roots_are_untracked() {
        let root = std::path::PathBuf::from("/tmp/project-a");
        let url = Url::parse("file:///tmp/project-a/src/bad.mw").unwrap();
        let mut tracker = DiagnosticsTracker::new();

        let epoch = tracker.bump_root_epoch(&root);
        let mut diagnostics = std::collections::HashMap::from([(url.clone(), non_empty())]);
        assert!(tracker.reconcile_success(&root, epoch, &mut diagnostics));

        assert_eq!(
            tracker.tracked_root_for_path(std::path::Path::new("/tmp/project-a/src/bad.mw")),
            Some(root)
        );
        assert_eq!(
            tracker.tracked_root_for_path(std::path::Path::new("/tmp/scratch.mw")),
            None,
            "a no-project scratch file must not be treated as belonging to a tracked project"
        );
    }

    #[test]
    fn diagnostics_tracker_stale_epoch_drops_result_for_that_root() {
        let root = std::path::PathBuf::from("/tmp/project-a");
        let url = Url::parse("file:///tmp/project-a/src/app.mw").unwrap();
        let mut tracker = DiagnosticsTracker::new();

        let stale_epoch = tracker.epoch(&root);
        let current_epoch = tracker.bump_root_epoch(&root);
        assert!(tracker.is_stale(&root, stale_epoch));
        assert!(!tracker.is_stale(&root, current_epoch));

        let mut stale_result = std::collections::HashMap::from([(url.clone(), non_empty())]);
        assert!(!tracker.reconcile_success(&root, stale_epoch, &mut stale_result));
        assert!(
            !tracker.is_tracked(&root, &url),
            "stale results should not replace current tracked diagnostics"
        );
    }

    #[test]
    fn diagnostics_tracker_clear_root_removes_only_that_root() {
        let root_a = std::path::PathBuf::from("/tmp/project-a");
        let root_b = std::path::PathBuf::from("/tmp/project-b");
        let url_a = Url::parse("file:///tmp/project-a/src/bad.mw").unwrap();
        let url_b = Url::parse("file:///tmp/project-b/src/bad.mw").unwrap();
        let mut tracker = DiagnosticsTracker::new();

        let epoch_a = tracker.bump_root_epoch(&root_a);
        let mut first = std::collections::HashMap::from([(url_a.clone(), non_empty())]);
        assert!(tracker.reconcile_success(&root_a, epoch_a, &mut first));
        let epoch_b = tracker.bump_root_epoch(&root_b);
        let mut second = std::collections::HashMap::from([(url_b.clone(), non_empty())]);
        assert!(tracker.reconcile_success(&root_b, epoch_b, &mut second));

        assert_eq!(tracker.clear_root(&root_a), vec![url_a.clone()]);

        assert!(!tracker.is_tracked(&root_a, &url_a));
        assert!(tracker.is_tracked(&root_b, &url_b));
    }

    #[test]
    fn diagnostics_tracker_clear_root_preserves_epoch() {
        let root = std::path::PathBuf::from("/tmp/project-a");
        let url = Url::parse("file:///tmp/project-a/src/bad.mw").unwrap();
        let mut tracker = DiagnosticsTracker::new();

        let stale_epoch = tracker.bump_root_epoch(&root);
        let mut diagnostics = std::collections::HashMap::from([(url.clone(), non_empty())]);
        assert!(tracker.reconcile_success(&root, stale_epoch, &mut diagnostics));
        let current_epoch = tracker.bump_root_epoch(&root);

        assert_eq!(tracker.clear_root(&root), vec![url.clone()]);

        assert!(tracker.is_stale(&root, stale_epoch));
        assert!(!tracker.is_stale(&root, current_epoch));
        assert!(!tracker.is_tracked(&root, &url));
    }

    #[test]
    fn diagnostics_tracker_clear_url_removes_only_that_url_under_root() {
        let root = std::path::PathBuf::from("/tmp/project-a");
        let bad = Url::parse("file:///tmp/project-a/src/bad.mw").unwrap();
        let worse = Url::parse("file:///tmp/project-a/src/worse.mw").unwrap();
        let mut tracker = DiagnosticsTracker::new();

        let epoch = tracker.bump_root_epoch(&root);
        let mut first = std::collections::HashMap::from([
            (bad.clone(), non_empty()),
            (worse.clone(), non_empty()),
        ]);
        assert!(tracker.reconcile_success(&root, epoch, &mut first));

        assert!(tracker.clear_url(&root, &bad));
        assert!(!tracker.clear_url(&root, &bad));
        assert!(!tracker.is_tracked(&root, &bad));
        assert!(tracker.is_tracked(&root, &worse));
    }

    fn non_empty() -> Vec<Diagnostic> {
        vec![Diagnostic::default()]
    }
}
