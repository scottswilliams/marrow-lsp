//! The Marrow Language Server: an LSP transport over stdio.
//!
//! All language intelligence lives in `marrow-lsp-core`; this binary is the thin
//! stdio transport over it. It keeps the open-buffer store and the project
//! workspace, and on each edit recomputes project diagnostics — reflecting unsaved
//! buffers — and publishes them. A recompute is debounced and dropped if a newer
//! edit lands while it waits, so a burst of keystrokes checks the project once.

mod backend;

use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf, Stdin};
use tower_lsp::{LspService, Server};

use backend::Backend;

#[tokio::main]
async fn main() {
    eprintln!(
        "marrow-lsp: starting (version {})",
        env!("CARGO_PKG_VERSION")
    );

    let stdin = ExitWatcher::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();

    // The standard LSP methods come from the `LanguageServer` impl; the Data
    // Explorer's three reads and the schema-change-impact advisory are registered
    // as custom methods over the same service so one transport serves both.
    let (service, socket) = LspService::build(Backend::new)
        .custom_method("marrow/savedRoots", Backend::saved_roots)
        .custom_method("marrow/savedChildren", Backend::saved_children)
        .custom_method("marrow/savedGet", Backend::saved_get)
        .custom_method("marrow/dataIntegrity", Backend::data_integrity)
        .finish();
    Server::new(stdin, stdout, socket).serve(service).await;

    // Reached only when stdin closes (EOF) without an `exit` notification — an
    // editor that dropped the pipe. The `exit` path terminates inside the watcher.
    eprintln!("marrow-lsp: stdin closed, stopping");
}

/// tower-lsp 0.20 routes the LSP `exit` notification through an internal layer
/// that flips the service to its "exited" state but does not stop the serve loop:
/// the loop stays parked reading stdin and only returns on EOF. An editor holds
/// the pipe open after `exit`, so the process would hang. This wrapper forwards
/// stdin verbatim to tower-lsp (which keeps its own framing and method routing)
/// and, watching the same bytes, exits the process the moment a complete `exit`
/// notification frame has passed through — code 0, the spec's clean teardown.
///
/// Detection reuses the LSP framing the protocol already mandates: a
/// `Content-Length` header, a blank line, then exactly that many body bytes. The
/// exit frame is forwarded before the process ends so nothing downstream is lost.
struct ExitWatcher {
    inner: Stdin,
    /// Bytes seen but not yet consumed as complete frames, so a frame split across
    /// reads is reassembled before its method is inspected.
    buffer: Vec<u8>,
}

impl ExitWatcher {
    fn new(inner: Stdin) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
        }
    }

    /// Scan the buffer for whole LSP frames; exit the process if one is an `exit`
    /// notification. Incomplete trailing bytes stay buffered for the next read.
    fn scan(&mut self) {
        loop {
            let Some((header_len, body_len)) = parse_header(&self.buffer) else {
                return; // Header not yet complete; wait for more bytes.
            };
            let frame_len = header_len + body_len;
            if self.buffer.len() < frame_len {
                return; // Body not yet fully read; wait for more bytes.
            }
            let body = &self.buffer[header_len..frame_len];
            if is_exit_notification(body) {
                // The frame has already been forwarded to tower-lsp by the read
                // that delivered it, so the service has seen `exit`; exit cleanly.
                eprintln!("marrow-lsp: exit notification received, stopping");
                std::process::exit(0);
            }
            self.buffer.drain(..frame_len);
        }
    }
}

impl AsyncRead for ExitWatcher {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let inner = Pin::new(&mut self.inner);
        let poll = inner.poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &poll {
            // Mirror exactly what tower-lsp received, then look for `exit`.
            let new = buf.filled()[before..].to_vec();
            if !new.is_empty() {
                self.buffer.extend_from_slice(&new);
                self.scan();
            }
        }
        poll
    }
}

/// The byte length of a complete LSP message header (up to and including the blank
/// line that ends it) and the `Content-Length` it declares, or `None` when the
/// header is not yet fully buffered. Only `Content-Length` is significant; other
/// header lines are skipped.
fn parse_header(buffer: &[u8]) -> Option<(usize, usize)> {
    let header_end = find_subslice(buffer, b"\r\n\r\n")? + 4;
    let header = &buffer[..header_end];
    let content_length = std::str::from_utf8(header)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length:"))?
        .trim()
        .parse::<usize>()
        .ok()?;
    Some((header_end, content_length))
}

/// Whether a JSON-RPC message body is the `exit` notification: `method` is `exit`
/// and there is no `id` (a notification, not a request). Parsing the body avoids
/// matching `exit` inside, say, a document's text.
fn is_exit_notification(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    value.get("method").and_then(|m| m.as_str()) == Some("exit") && value.get("id").is_none()
}

/// The index of the first occurrence of `needle` in `haystack`, or `None`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
