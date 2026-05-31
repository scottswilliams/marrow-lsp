//! The language server backend: open buffers, the project workspace, and the
//! edit-driven recompute that publishes diagnostics.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use marrow_lsp_core::data_explorer::{
    SavedChildrenParams, SavedChildrenResult, SavedGetParams, SavedGetResult, SavedRootsResult,
    saved_children_with_schema, saved_get, saved_roots,
};
use marrow_lsp_core::data_integrity::{DataIntegrityParams, DataIntegrityResult, data_integrity};
use marrow_lsp_core::diagnostics::snapshot_to_diagnostics;
use marrow_lsp_core::documents::Documents;
use marrow_lsp_core::navigation::{RenameError, SnapshotIndices};
use marrow_lsp_core::positions::Position as CorePosition;
use marrow_lsp_core::store::StoreReader;
use marrow_lsp_core::workspace::{AnalysisSnapshot, Workspace, url_to_path};
use marrow_lsp_core::{
    code_lens, completion, formatting, hover, navigation, semantic_tokens, signature_help, symbols,
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
    /// Whether to read real stored data for hover live values, the record-count
    /// lens, and the Data Explorer. Mirrors the `marrow.liveData` setting, default
    /// on; turning it off keeps the server from ever opening the store.
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
            live_data: Arc::new(AtomicBool::new(true)),
        }
    }

    /// The store reader for the resolved project, or `None` when live data is off,
    /// no project is resolved yet, or the project pins no native store. The reader
    /// is short-lived: it holds only the store path and opens the file per read.
    fn reader(&self, workspace: &Workspace) -> Option<StoreReader> {
        if !self.live_data.load(Ordering::Relaxed) {
            return None;
        }
        StoreReader::for_project(workspace.project()?)
    }

    /// `marrow/savedRoots`: the project's saved root names. A short, capped store
    /// read; never recomputes the project.
    pub async fn saved_roots(&self) -> jsonrpc::Result<SavedRootsResult> {
        let state = self.state.lock().await;
        let reader = self.reader(&state.workspace);
        Ok(saved_roots(reader.as_ref()))
    }

    /// `marrow/savedChildren`: the immediate children of a saved path, capped.
    pub async fn saved_children(
        &self,
        params: SavedChildrenParams,
    ) -> jsonrpc::Result<SavedChildrenResult> {
        let state = self.state.lock().await;
        let reader = self.reader(&state.workspace);
        let empty = EMPTY_PROGRAM.get_or_init(Default::default);
        let program = state.workspace.program().unwrap_or(empty);
        Ok(saved_children_with_schema(params, program, reader.as_ref()))
    }

    /// `marrow/savedGet`: the presence and schema-typed value at a saved path. The
    /// path is typed against the cached snapshot's program; no recompute.
    pub async fn saved_get(&self, params: SavedGetParams) -> jsonrpc::Result<SavedGetResult> {
        let state = self.state.lock().await;
        let reader = self.reader(&state.workspace);
        let empty = EMPTY_PROGRAM.get_or_init(Default::default);
        let program = state.workspace.program().unwrap_or(empty);
        Ok(saved_get(params, program, reader.as_ref()))
    }

    /// `marrow/dataIntegrity`: the schema-change-impact advisory. A capped,
    /// on-demand scan of the project's saved data that flags every record the
    /// current schema can no longer account for. Reads through the same
    /// `marrow.liveData`-gated reader as hover live values and the Data Explorer,
    /// so it never opens the store when live data is off, and it is invoked only on
    /// explicit request — never on save or edit. A `None` reader (live data off, no
    /// project, or no native store) or an unreadable store answers
    /// `available: false`. The path is typed against the cached snapshot's program;
    /// no recompute.
    pub async fn data_integrity(
        &self,
        _params: DataIntegrityParams,
    ) -> jsonrpc::Result<DataIntegrityResult> {
        let state = self.state.lock().await;
        let reader = self.reader(&state.workspace);
        let empty = EMPTY_PROGRAM.get_or_init(Default::default);
        let program = state.workspace.program().unwrap_or(empty);
        Ok(data_integrity(reader.as_ref(), program))
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

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        // `marrow.liveData` (default true) gates reading real stored data. The
        // client passes it under `initializationOptions`; only an explicit `false`
        // turns it off, so a missing or malformed option keeps the default on.
        if let Some(options) = &params.initialization_options
            && options.get("marrow.liveData") == Some(&serde_json::Value::Bool(false))
        {
            self.live_data.store(false, Ordering::Relaxed);
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
                // Record-count lenses resolve their count lazily on demand.
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: Some(true),
                }),
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
        // Build and cache the binding index (once per recompute) so the docs
        // lookup reuses it exactly like definition and references do — never
        // rebuilding resolution per hover. This also ensures the snapshot.
        if ensure_index(workspace, documents, &path).is_none() {
            return Ok(None);
        }
        // The reader borrows the project from the workspace; resolve it before
        // re-borrowing the snapshot so all borrows are read-only and disjoint.
        let reader = self.reader(workspace);
        let snapshot = workspace.latest().expect("ensured just above");
        let index = workspace
            .binding_index_cached()
            .expect("ensured just above");
        Ok(hover::hover_with_index(
            snapshot,
            index,
            &path,
            offset,
            reader.as_ref(),
        ))
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
        if ensure_index(workspace, documents, &path).is_none() {
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
        if ensure_index(workspace, documents, &path).is_none() {
            return Ok(None);
        }
        let indices = snapshot_indices(workspace, documents);
        let index = workspace
            .binding_index_cached()
            .expect("ensured just above");
        Ok(navigation::references(
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
        if ensure_index(workspace, documents, &path).is_none() {
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
        if ensure_index(workspace, documents, &path).is_none() {
            return Ok(None);
        }
        let indices = snapshot_indices(workspace, documents);
        let index = workspace
            .binding_index_cached()
            .expect("ensured just above");
        match navigation::rename(index, &indices, &path, offset, &new_name) {
            Ok(edit) => Ok(Some(edit)),
            // No symbol under the cursor is not an error — the editor simply does
            // nothing. A saved-data-backed symbol is refused with a clear message so
            // the user learns renaming it would orphan stored records.
            Err(RenameError::NoSymbol) => Ok(None),
            Err(error @ (RenameError::SavedDataBacked { .. } | RenameError::InvalidName)) => {
                Err(jsonrpc::Error::invalid_params(error.message()))
            }
        }
    }

    /// The record-count lenses for a saved-resource file: one per resource with a
    /// saved root, unresolved (no count yet). Reads only the cached parse; the
    /// count is filled in lazily on resolve.
    async fn code_lens(&self, params: CodeLensParams) -> jsonrpc::Result<Option<Vec<CodeLens>>> {
        let url = params.text_document.uri;
        let state = self.state.lock().await;
        let Some(document) = state.documents.get(&url) else {
            return Ok(None);
        };
        Ok(Some(code_lens::code_lenses(
            &document.parsed.file,
            &document.index,
        )))
    }

    /// Resolve a record-count lens: read the live count for its root and set the
    /// display command. An unavailable store leaves the lens quiet. One short,
    /// capped store read; never recomputes the project.
    async fn code_lens_resolve(&self, lens: CodeLens) -> jsonrpc::Result<CodeLens> {
        let state = self.state.lock().await;
        let reader = self.reader(&state.workspace);
        Ok(code_lens::resolve(lens, reader.as_ref()))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> jsonrpc::Result<Option<DocumentSymbolResponse>> {
        let url = params.text_document.uri;
        let Some(path) = url_to_path(&url) else {
            return Ok(None);
        };

        let mut state = self.state.lock().await;
        let State {
            documents,
            workspace,
        } = &mut *state;
        // documentSymbol is only sent for an open document, so a missing buffer is
        // a request the client should not have made; outline nothing rather than
        // reach to disk for text the editor already holds.
        if documents.get(&url).is_none() {
            return Ok(None);
        }
        let Some(snapshot) = ensure_snapshot(workspace, documents, &path) else {
            return Ok(None);
        };
        let Some(analyzed) = snapshot.files.iter().find(|file| file.path == path) else {
            return Ok(None);
        };

        // Outline against the open buffer's index so selection ranges land on the
        // unsaved text the user sees.
        let Some(document) = documents.get(&url) else {
            return Ok(None);
        };
        let outline = symbols::document_symbols(&analyzed.parsed.file, &document.index);
        Ok(Some(DocumentSymbolResponse::Nested(outline)))
    }

    async fn symbol(
        &self,
        _: WorkspaceSymbolParams,
    ) -> jsonrpc::Result<Option<Vec<SymbolInformation>>> {
        let mut state = self.state.lock().await;
        let State {
            documents,
            workspace,
        } = &mut *state;
        // Workspace symbols need a project; recompute against any open buffer to
        // resolve one, then flatten its modules.
        let Some(file) = documents.urls().filter_map(url_to_path).next() else {
            return Ok(workspace
                .latest()
                .map(|snapshot| symbols::workspace_symbols(&snapshot.program)));
        };
        let Some(snapshot) = ensure_snapshot(workspace, documents, &file) else {
            return Ok(None);
        };
        Ok(Some(symbols::workspace_symbols(&snapshot.program)))
    }

    /// Complete at the cursor. Reads only the document's cached lex/parse and the
    /// last cached snapshot's program — it never re-lexes, re-parses, or
    /// recomputes the project, so it answers instantly and even while the project
    /// has not yet been (re)checked. With no snapshot yet, an empty program backs
    /// the context-only modes (keywords, builtins) so the first keystroke still
    /// completes.
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
        let program = state.workspace.program().unwrap_or(empty);

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
    /// Reads the open document's cached lex/source, the last checked program for
    /// callable shape, and the latest snapshot only for optional documentation.
    /// This keeps in-progress argument lists useful before they parse.
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

        let Some(program) = state.workspace.program() else {
            return Ok(None);
        };
        Ok(signature_help::signature_help(
            program,
            state.workspace.latest(),
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
        let binding_index = if snapshot_has_document_text(workspace.latest(), &path, &document.text)
        {
            workspace.binding_index()
        } else {
            None
        };
        let data = semantic_tokens::semantic_tokens_with_bindings(
            &document.lexed,
            &document.parsed.file,
            &document.index,
            binding_index.map(|index| (index, path.as_path())),
        );
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

/// The cached analysis, recomputing it once against `file` if none exists yet, so
/// a request that arrives before any debounced recompute still has a snapshot. A
/// file outside any project yields `None`.
fn ensure_snapshot<'a>(
    workspace: &'a mut Workspace,
    documents: &Documents,
    file: &std::path::Path,
) -> Option<&'a AnalysisSnapshot> {
    if workspace.latest().is_none() {
        let _ = workspace.recompute(file, documents);
    }
    workspace.latest()
}

/// Ensure a snapshot exists, then build and cache its binding index, so a later
/// immutable borrow can read it alongside the snapshot. Returns `None` for a file
/// outside any project. The index is built at most once per recompute; subsequent
/// navigation requests reuse the cache and this is a no-op.
fn ensure_index<'a>(
    workspace: &'a mut Workspace,
    documents: &Documents,
    file: &std::path::Path,
) -> Option<&'a marrow_lsp_core::workspace::BindingIndex> {
    if workspace.latest().is_none() {
        let _ = workspace.recompute(file, documents);
    }
    workspace.binding_index()
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
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
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
}
