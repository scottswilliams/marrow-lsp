//! The language server backend: open buffers, the project workspace, and the
//! edit-driven recompute that publishes diagnostics.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use marrow_lsp_core::data_explorer::{
    DataChildViewsResponse, DataChildrenError, DataChildrenRequest, DataKeyDto, DataPresenceDto,
    DataReadRequest, DataReadResponse, DataRequestValidationError, SavedRootsResult, saved_roots,
    session_data_child_views_page, session_data_read, validate_data_children_request,
    validate_data_read_request,
};
use marrow_lsp_core::diagnostics::snapshot_to_diagnostics;
use marrow_lsp_core::documents::Documents;
use marrow_lsp_core::navigation::{RenameError, SnapshotIndices};
use marrow_lsp_core::positions::Position as CorePosition;
use marrow_lsp_core::store::{
    SavedDataSession, SavedDataWatchTargets, open_saved_data_session, saved_data_watch_targets,
};
use marrow_lsp_core::workspace::{AnalysisSnapshot, Workspace, url_to_path};
use marrow_lsp_core::{
    completion, formatting, hover, navigation, semantic_tokens, signature_help, symbols,
};
use tokio::sync::Mutex;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, jsonrpc};

/// How long to wait after an edit before checking the project, so a burst of
/// keystrokes settles into one recompute.
const DEBOUNCE: Duration = Duration::from_millis(150);

pub struct Backend {
    client: Client,
    state: Arc<Mutex<State>>,
    /// Bumped on every buffer edit. A debounced recompute captures this and only
    /// runs if it is still current when the debounce elapses, so a newer edit
    /// supersedes an in-flight recompute.
    edit_seq: Arc<AtomicU64>,
    /// Whether to read real stored data for saved-data requests.
    /// Mirrors the `marrow.liveData` setting, default off; enabling it permits
    /// the server to open the native dev store read-only.
    live_data: Arc<AtomicBool>,
}

struct State {
    documents: Documents,
    workspace: Workspace,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(Mutex::new(State {
                documents: Documents::new(),
                workspace: Workspace::new(),
            })),
            edit_seq: Arc::new(AtomicU64::new(0)),
            live_data: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The Marrow read session for saved-data inspection, only when the cached
    /// analysis still matches the editor-visible source.
    fn data_session(&self, state: &State) -> Option<SavedDataSession> {
        if !self.live_data.load(Ordering::Relaxed) {
            return None;
        }
        let fresh_program = state.workspace.fresh_program(&state.documents)?;
        open_saved_data_session(state.workspace.project()?)
            .ok()
            .flatten()
            .filter(|session| session.matches_checked_program(fresh_program))
    }

    /// `marrow/savedRoots`: the project's saved root views. Requires a fresh
    /// checked program and never recomputes the project.
    pub async fn saved_roots(&self) -> jsonrpc::Result<SavedRootsResult> {
        let state = self.state.lock().await;
        let session = self.data_session(&state);
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
        let state = self.state.lock().await;
        let Some(session) = self.data_session(&state) else {
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
        let state = self.state.lock().await;
        let Some(session) = self.data_session(&state) else {
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
        let state = self.state.lock().await;
        saved_data_watch_targets(
            state.workspace.project(),
            self.live_data.load(Ordering::Relaxed),
        )
        .map_err(|error| {
            jsonrpc::Error::invalid_params(format!("invalid project store config: {error}"))
        })
    }

    /// Note that an edit happened and schedule a debounced recompute for `file`.
    /// The recompute is dropped if another edit lands before the debounce elapses.
    fn schedule_recompute(&self, file: std::path::PathBuf) {
        let seq = self.edit_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let state = Arc::clone(&self.state);
        let edit_seq = Arc::clone(&self.edit_seq);
        let client = self.client.clone();
        tokio::spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            // A newer edit has been scheduled; let it do the work instead.
            if edit_seq.load(Ordering::SeqCst) != seq {
                return;
            }
            Self::recompute_and_publish(&state, &client, &file).await;
        });
    }

    /// Recompute the project against the current buffers and publish diagnostics
    /// for every open document. A superseded edit is already dropped before this
    /// runs (the edit-sequence check in `schedule_recompute`); holding the state
    /// lock across the mapping keeps a concurrent recompute from interleaving a
    /// partial publish.
    async fn recompute_and_publish(state: &Mutex<State>, client: &Client, file: &std::path::Path) {
        let published = {
            let mut state = state.lock().await;
            let State {
                documents,
                workspace,
            } = &mut *state;
            match workspace.recompute(file, documents) {
                Ok(snapshot) => {
                    let urls: Vec<Url> = documents.urls().cloned().collect();
                    Some(snapshot_to_diagnostics(snapshot, &urls, |url| {
                        documents.get(url).map(|document| &document.index)
                    }))
                }
                // A buffer outside any project, or a project that cannot be walked,
                // has nothing to publish; the editor keeps whatever it last showed.
                // A file simply outside any project is the common, quiet case; a
                // malformed config or an unwalkable source root is a real failure
                // the user should be able to see in the output channel.
                Err(error) => {
                    if !matches!(error, marrow_lsp_core::workspace::WorkspaceError::NoProject) {
                        eprintln!(
                            "marrow-lsp: recompute failed for {}: {error}",
                            file.display()
                        );
                    }
                    None
                }
            }
        };

        if let Some(published) = published {
            for (url, diagnostics) in published {
                client.publish_diagnostics(url, diagnostics, None).await;
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
                ..ServerCapabilities::default()
            },
        })
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        let url = document.uri;
        {
            let mut state = self.state.lock().await;
            state.documents.open(url.clone(), document.text);
        }
        if let Some(path) = url_to_path(&url) {
            self.schedule_recompute(path);
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let url = params.text_document.uri;
        // Full-text sync: the client sends the whole document as a single change.
        let Some(change) = params.content_changes.into_iter().next_back() else {
            return;
        };
        {
            let mut state = self.state.lock().await;
            state.documents.change(&url, change.text);
        }
        if let Some(path) = url_to_path(&url) {
            self.schedule_recompute(path);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let url = params.text_document.uri;
        let path = url_to_path(&url);
        {
            let mut state = self.state.lock().await;
            state.documents.close(&url);
        }
        // A closed buffer's problems no longer apply to the editor's view.
        self.client.publish_diagnostics(url, Vec::new(), None).await;
        if let Some(path) = path {
            self.schedule_recompute(path);
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let changed_paths: Vec<std::path::PathBuf> = params
            .changes
            .iter()
            .filter_map(|event| url_to_path(&event.uri))
            .collect();
        if changed_paths.is_empty() {
            return;
        }

        let recompute_path = {
            let mut state = self.state.lock().await;
            let Some(change) = watched_project_change(&state.workspace, &changed_paths) else {
                return;
            };
            match change {
                WatchedProjectChange::ProjectConfig(path) => {
                    state.workspace.invalidate_project();
                    path
                }
                WatchedProjectChange::AnalysisFile(path) => {
                    state.workspace.invalidate_analysis();
                    path
                }
            }
        };

        self.schedule_recompute(recompute_path);
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let position = params.text_document_position_params;
        let url = position.text_document.uri;
        let Some(path) = url_to_path(&url) else {
            return Ok(None);
        };

        let mut state = self.state.lock().await;
        // The cursor's byte offset comes from this buffer's index, so a request
        // that arrives before the debounced recompute still maps correctly.
        let Some(offset) = state
            .documents
            .get(&url)
            .map(|document| document.index.offset(to_core_position(position.position)))
        else {
            return Ok(None);
        };

        let State {
            documents,
            workspace,
        } = &mut *state;
        let Some(document) = documents.get(&url) else {
            return Ok(None);
        };
        // Build and cache the binding index (once per recompute) so the docs
        // lookup reuses it exactly like definition and references do.
        if ensure_fresh_index(workspace, documents, &path, &document.text).is_none() {
            return Ok(None);
        }
        let snapshot = workspace.latest().expect("ensured just above");
        let index = workspace
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

        let mut state = self.state.lock().await;
        let Some(offset) = offset_in(&state.documents, &url, position.position) else {
            return Ok(None);
        };
        let State {
            documents,
            workspace,
        } = &mut *state;
        let Some(document) = documents.get(&url) else {
            return Ok(None);
        };
        if ensure_fresh_index(workspace, documents, &path, &document.text).is_none() {
            return Ok(None);
        }
        let indices = snapshot_indices(workspace, documents);
        let snapshot = workspace.latest().expect("ensured just above");
        let index = workspace
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

        let mut state = self.state.lock().await;
        let Some(offset) = offset_in(&state.documents, &url, position.position) else {
            return Ok(None);
        };
        let State {
            documents,
            workspace,
        } = &mut *state;
        let Some(document) = documents.get(&url) else {
            return Ok(None);
        };
        if ensure_fresh_index(workspace, documents, &path, &document.text).is_none() {
            return Ok(None);
        }
        let indices = snapshot_indices(workspace, documents);
        let snapshot = workspace.latest().expect("ensured just above");
        let index = workspace
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

        let mut state = self.state.lock().await;
        let Some(offset) = offset_in(&state.documents, &url, params.position) else {
            return Ok(None);
        };
        let State {
            documents,
            workspace,
        } = &mut *state;
        let Some(document) = documents.get(&url) else {
            return Ok(None);
        };
        if ensure_fresh_index(workspace, documents, &path, &document.text).is_none() {
            return Ok(None);
        }
        let indices = snapshot_indices(workspace, documents);
        let index = workspace
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

        let mut state = self.state.lock().await;
        let Some(offset) = offset_in(&state.documents, &url, position.position) else {
            return Ok(None);
        };
        let State {
            documents,
            workspace,
        } = &mut *state;
        let Some(document) = documents.get(&url) else {
            return Ok(None);
        };
        if ensure_fresh_index(workspace, documents, &path, &document.text).is_none() {
            return Ok(None);
        }
        let indices = snapshot_indices(workspace, documents);
        let index = workspace
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

        let state = self.state.lock().await;
        // documentSymbol is only sent for an open document, so a missing buffer is
        // a request the client should not have made; outline nothing rather than
        // reach to disk for text the editor already holds.
        let Some(document) = state.documents.get(&url) else {
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
        let mut state = self.state.lock().await;
        let State {
            documents,
            workspace,
        } = &mut *state;
        // Workspace symbols need a project; recompute against any open buffer to
        // resolve one, then flatten its checked and catalog-backed declarations.
        let Some(file) = documents.urls().filter_map(url_to_path).next() else {
            return Ok(workspace
                .fresh_latest(documents)
                .map(|snapshot| symbols::workspace_symbols(snapshot, &search_text)));
        };
        if workspace.latest().is_none() {
            let _ = workspace.recompute(&file, documents);
        }
        let Some(snapshot) = workspace.fresh_latest(documents) else {
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

        let state = self.state.lock().await;
        let Some(document) = state.documents.get(&url) else {
            return Ok(None);
        };
        let offset = document.index.offset(to_core_position(position.position));

        let empty = EMPTY_PROGRAM.get_or_init(Default::default);
        let program = state
            .workspace
            .fresh_program_or_empty(&state.documents)
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

        let state = self.state.lock().await;
        let Some(document) = state.documents.get(&url) else {
            return Ok(None);
        };
        let offset = document.index.offset(to_core_position(position.position));

        let Some(snapshot) = state.workspace.fresh_latest(&state.documents) else {
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
        let mut state = self.state.lock().await;
        let State {
            documents,
            workspace,
        } = &mut *state;
        let Some(document) = documents.get(&url) else {
            return Ok(None);
        };
        let has_fresh_analysis =
            snapshot_has_document_text(workspace.latest(), &path, &document.text)
                && workspace.latest_matches_sources(documents);
        if has_fresh_analysis {
            let _ = workspace.binding_index();
        }
        let data = if let (Some(snapshot), Some(binding_index)) = (
            has_fresh_analysis.then(|| workspace.latest()).flatten(),
            has_fresh_analysis
                .then(|| workspace.binding_index_cached())
                .flatten(),
        ) {
            semantic_tokens::semantic_tokens_for_file(
                snapshot,
                binding_index,
                path.as_path(),
                &document.index,
            )
            .unwrap_or_else(|| {
                semantic_tokens::semantic_tokens(&document.lexed, &document.parsed, &document.index)
            })
        } else {
            semantic_tokens::semantic_tokens(&document.lexed, &document.parsed, &document.index)
        };
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
        let state = self.state.lock().await;
        let Some(document) = state.documents.get(&url) else {
            return Ok(None);
        };
        Ok(formatting::formatting(
            &document.text,
            &document.parsed,
            &document.index,
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

/// Ensure a parse-clean snapshot exists, then build and cache its binding index
/// only when the target file and every analyzed source still matches the
/// editor-visible source world.
/// Returns `None` for stale text or for a file outside any project.
fn ensure_fresh_index<'a>(
    workspace: &'a mut Workspace,
    documents: &Documents,
    file: &std::path::Path,
    text: &str,
) -> Option<&'a marrow_lsp_core::workspace::BindingIndex> {
    if workspace.latest().is_none() {
        let _ = workspace.recompute(file, documents);
    }
    let snapshot = workspace.latest()?;
    if !snapshot_has_document_text(Some(snapshot), file, text) {
        return None;
    }
    if snapshot_has_parse_errors(snapshot) {
        return None;
    }
    if !workspace.latest_matches_sources(documents) {
        return None;
    }
    workspace.binding_index()
}

fn snapshot_has_parse_errors(snapshot: &AnalysisSnapshot) -> bool {
    snapshot.files.iter().any(|file| file.parsed.has_errors())
}

enum WatchedProjectChange {
    ProjectConfig(std::path::PathBuf),
    AnalysisFile(std::path::PathBuf),
}

fn watched_project_change(
    workspace: &Workspace,
    changed_paths: &[std::path::PathBuf],
) -> Option<WatchedProjectChange> {
    let project = workspace.project()?;
    let config_path = project.root.join("marrow.json");
    if let Some(path) = changed_paths.iter().find(|path| *path == &config_path) {
        return Some(WatchedProjectChange::ProjectConfig(path.clone()));
    }
    let lock_path = project
        .root
        .join(marrow_lsp_core::workspace::COMMITTED_LOCK_FILE);
    if let Some(path) = changed_paths.iter().find(|path| *path == &lock_path) {
        return Some(WatchedProjectChange::AnalysisFile(path.clone()));
    }
    changed_paths
        .iter()
        .find(|path| project.analyzes_file(path) || snapshot_has_file(workspace.latest(), path))
        .cloned()
        .map(WatchedProjectChange::AnalysisFile)
}

fn snapshot_has_file(snapshot: Option<&AnalysisSnapshot>, path: &std::path::Path) -> bool {
    snapshot.is_some_and(|snapshot| snapshot.files.iter().any(|file| file.path == path))
}

/// The byte offset of `position` in the buffer at `url`, from that buffer's cached
/// index, so a request maps correctly even before the debounced recompute lands.
fn offset_in(documents: &Documents, url: &Url, position: Position) -> Option<usize> {
    documents
        .get(url)
        .map(|document| document.index.offset(to_core_position(position)))
}

/// Build the per-file index resolver navigation maps spans through: one
/// [`LineIndex`](marrow_lsp_core::positions::LineIndex) per file the snapshot
/// analyzed, the open buffer's when held (so spans land on unsaved text), else one
/// built from disk. Called once per navigation request over the bounded project
/// file set.
fn snapshot_indices<'a>(workspace: &'a Workspace, documents: &'a Documents) -> SnapshotIndices<'a> {
    let files = workspace
        .latest()
        .map(|snapshot| snapshot.files.iter().map(|file| file.path.as_path()))
        .into_iter()
        .flatten();
    SnapshotIndices::new(files, |path| {
        marrow_lsp_core::diagnostics::path_to_url(path)
            .and_then(|url| documents.get(&url))
            .map(|document| &document.index)
    })
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
        documents.open(url.clone(), clean.to_string());
        let mut workspace = Workspace::new();
        workspace.recompute(&file, &documents).unwrap();

        assert!(snapshot_has_document_text(workspace.latest(), &file, clean));

        let edited = "module m\n\nfn f(): int\n    return 2\n";
        documents.change(&url, edited.to_string());
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
        documents.open(a_url, a_source.to_string());
        documents.open(b_url.clone(), b_source.to_string());
        let mut workspace = Workspace::new();
        workspace.recompute(&a, &documents).unwrap();

        assert!(workspace.latest_matches_sources(&documents));

        documents.change(
            &b_url,
            "module b\n\nfn g(): int\n    return 3\n".to_string(),
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
        documents.open(app_url, app_source.to_string());
        let mut workspace = Workspace::new();
        workspace.recompute(&app, &documents).unwrap();

        assert!(workspace.latest_matches_sources(&documents));

        let new_defs = a_src.join("defs.mw");
        let defs_url = Url::from_file_path(&new_defs).unwrap();
        documents.open(
            defs_url,
            "module defs\n\npub fn answer(): int\n    return 1\n".to_string(),
        );

        assert!(
            !workspace.latest_matches_sources(&documents),
            "a new open source-root file absent from the cached snapshot can change project bindings"
        );
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
        let change = watched_project_change(&workspace, std::slice::from_ref(&lock_path));

        match change {
            Some(WatchedProjectChange::AnalysisFile(path)) => assert_eq!(path, lock_path),
            Some(WatchedProjectChange::ProjectConfig(path)) => {
                panic!("committed lock should not invalidate project config: {path:?}")
            }
            None => panic!("committed lock should invalidate analysis"),
        }
    }
}
