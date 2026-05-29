//! The Marrow Language Server: an LSP transport over stdio.
//!
//! All language intelligence lives in `marrow-lsp-core`; this binary is the thin
//! stdio transport over it. It keeps the open-buffer store and the project
//! workspace, and on each edit recomputes project diagnostics — reflecting unsaved
//! buffers — and publishes them. A recompute is debounced and dropped if a newer
//! edit lands while it waits, so a burst of keystrokes checks the project once.

mod backend;

use tower_lsp::{LspService, Server};

use backend::Backend;

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
