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

    // The standard LSP methods come from the `LanguageServer` impl; the Data
    // Explorer's three reads are registered as custom methods over the same
    // service so one transport serves both.
    let (service, socket) = LspService::build(Backend::new)
        .custom_method("marrow/savedRoots", Backend::saved_roots)
        .custom_method("marrow/savedChildren", Backend::saved_children)
        .custom_method("marrow/savedGet", Backend::saved_get)
        .finish();
    Server::new(stdin, stdout, socket).serve(service).await;
}
