//! The language server backend: open buffers, the project workspace, and the
//! edit-driven recompute that publishes diagnostics.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use marrow_lsp_core::diagnostics::snapshot_to_diagnostics;
use marrow_lsp_core::documents::Documents;
use marrow_lsp_core::positions::{LineIndex, Position as CorePosition};
use marrow_lsp_core::workspace::{AnalysisSnapshot, Workspace, url_to_path};
use marrow_lsp_core::{hover, symbols};
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
        }
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
                Err(_) => None,
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
    async fn initialize(&self, _: InitializeParams) -> jsonrpc::Result<InitializeResult> {
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
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
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
            state
                .documents
                .open(url.clone(), document.version, document.text);
        }
        if let Some(path) = url_to_path(&url) {
            self.schedule_recompute(path);
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let url = params.text_document.uri;
        let version = params.text_document.version;
        // Full-text sync: the client sends the whole document as a single change.
        let Some(change) = params.content_changes.into_iter().next_back() else {
            return;
        };
        {
            let mut state = self.state.lock().await;
            state.documents.change(&url, version, change.text);
        }
        if let Some(path) = url_to_path(&url) {
            self.schedule_recompute(path);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let url = params.text_document.uri;
        {
            let mut state = self.state.lock().await;
            state.documents.close(&url);
        }
        // A closed buffer's problems no longer apply to the editor's view.
        self.client.publish_diagnostics(url, Vec::new(), None).await;
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
        let Some(snapshot) = ensure_snapshot(workspace, documents, &path) else {
            return Ok(None);
        };
        Ok(hover::hover(snapshot, &path, offset))
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
        let Some(snapshot) = ensure_snapshot(workspace, documents, &path) else {
            return Ok(None);
        };
        let Some(analyzed) = snapshot.files.iter().find(|file| file.path == path) else {
            return Ok(None);
        };

        // Outline against the open buffer's index when present, else the parse's
        // own text rebuilt into an index, so selection ranges land on the buffer.
        let outline = match documents.get(&url) {
            Some(document) => symbols::document_symbols(&analyzed.parsed.file, &document.index),
            None => symbols::document_symbols(
                &analyzed.parsed.file,
                &LineIndex::new(read_to_string(&path)),
            ),
        };
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
}

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

fn read_to_string(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}
