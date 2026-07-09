//! The Marrow Language Server: an LSP transport over stdio.
//!
//! All language intelligence lives in `marrow-lsp-core`; this binary is the thin
//! stdio transport over it. It keeps the open-buffer store and the project
//! workspace, and on each edit recomputes project diagnostics — reflecting unsaved
//! buffers — and publishes them. A recompute is debounced and dropped if a newer
//! edit lands while it waits, so a burst of keystrokes checks the project once.

mod backend;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use marrow_terminal::{Style, paint_stderr};
use tokio::io::{AsyncRead, ReadBuf, Stdin};
use tower_lsp::{LspService, Server};

use backend::Backend;

pub(crate) fn stderr_notice(label: &str) -> String {
    paint_stderr(Style::Notice, label)
}

pub(crate) fn stderr_error(label: &str) -> String {
    paint_stderr(Style::Error, label)
}

#[tokio::main]
async fn main() {
    eprintln!(
        "{} starting (version {})",
        stderr_notice("marrow-lsp:"),
        env!("CARGO_PKG_VERSION")
    );

    let stdout = tokio::io::stdout();

    // The standard LSP methods come from the `LanguageServer` impl; saved-data
    // inspection is registered as custom methods over the same service so one
    // transport serves both.
    let (service, socket) = LspService::build(Backend::new)
        .custom_method("marrow/savedRoots", Backend::saved_roots)
        .custom_method("marrow/dataChildren", Backend::data_children)
        .custom_method("marrow/dataRead", Backend::data_read)
        .custom_method("marrow/dataWatchTargets", Backend::data_watch_targets)
        .finish();
    // Share the backend's shutdown-seen flag with the watcher so the `exit` code
    // reflects whether a clean `shutdown` handshake preceded it.
    let stdin = ExitWatcher::new(tokio::io::stdin(), service.inner().shutdown_seen());
    Server::new(stdin, stdout, socket).serve(service).await;

    // Reached only when stdin closes (EOF) without an `exit` notification — an
    // editor that dropped the pipe. The `exit` path terminates inside the watcher.
    eprintln!("{} stdin closed, stopping", stderr_notice("marrow-lsp:"));
}

/// tower-lsp 0.20 routes the LSP `exit` notification through an internal layer
/// that flips the service to its "exited" state but does not stop the serve loop:
/// the loop stays parked reading stdin and only returns on EOF. An editor holds
/// the pipe open after `exit`, so the process would hang. This wrapper forwards
/// stdin verbatim to tower-lsp (which keeps its own framing and method routing)
/// and, watching the same bytes, exits the process the moment a complete `exit`
/// notification frame has passed through — code 0 when a clean `shutdown` preceded
/// it, code 1 otherwise, as the spec requires.
///
/// Detection reuses the LSP framing the protocol already mandates: a
/// `Content-Length` header, a blank line, then exactly that many body bytes. The
/// exit frame is forwarded before the process ends so nothing downstream is lost.
struct ExitWatcher {
    inner: Stdin,
    /// Bytes seen but not yet consumed as complete frames, so a frame split across
    /// reads is reassembled before its method is inspected.
    buffer: Vec<u8>,
    /// The backend's shutdown-seen flag, read when `exit` arrives to choose the exit
    /// code: 0 after a clean `shutdown`, 1 for an `exit` with no prior shutdown.
    shutdown_seen: Arc<AtomicBool>,
}

impl ExitWatcher {
    fn new(inner: Stdin, shutdown_seen: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
            shutdown_seen,
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
                self.exit_now();
            }
            self.buffer.drain(..frame_len);
        }
    }

    /// End the process on `exit`. The frame has already been forwarded to tower-lsp by
    /// the read that delivered it, so the service has seen `exit`. Per the LSP spec the
    /// code is 0 only when a clean `shutdown` preceded it, else 1.
    fn exit_now(&self) -> ! {
        // A conformant client awaits the `shutdown` response before sending `exit`, so
        // the shutdown handler's flag write happens-before this read and the code is 0.
        // A non-conformant client that pipelines `shutdown` and `exit` without awaiting
        // the response may have this read race the flag write and be scored unclean
        // (code 1); that is an acceptable outcome for a protocol violation, not a race
        // to engineer around.
        let clean = self.shutdown_seen.load(Ordering::SeqCst);
        let code = if clean { 0 } else { 1 };
        let reason = if clean {
            "clean shutdown"
        } else {
            "no prior shutdown"
        };
        eprintln!(
            "{} exit notification received ({reason}), stopping",
            stderr_notice("marrow-lsp:")
        );
        std::process::exit(code);
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
/// and there is no `id` (a notification, not a request). The full JSON parse confirms
/// it precisely; the cheap pre-guard keeps that parse off the hot path for the frames
/// that dominate a session — large `didChange` bodies carrying whole documents.
fn is_exit_notification(body: &[u8]) -> bool {
    if !is_exit_candidate(body) {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    value.get("method").and_then(|m| m.as_str()) == Some("exit") && value.get("id").is_none()
}

/// An `exit` notification's bytes literally contain the token `exit`; a body without
/// it cannot be `exit`, so it never reaches the full JSON parse. This substring guard
/// is the whole point: a document edit is not re-serialized to a `serde_json::Value`
/// just to be rejected. Length is deliberately not a factor — a conformant but verbose
/// `exit` (pretty-printed, `"params": null`, future fields) can exceed any tight cap,
/// and false-negating it would leave a client that holds stdin open hanging forever.
fn is_exit_candidate(body: &[u8]) -> bool {
    find_subslice(body, b"exit").is_some()
}

/// The index of the first occurrence of `needle` in `haystack`, or `None`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{is_exit_candidate, is_exit_notification};

    #[test]
    fn a_real_exit_frame_is_recognized() {
        let body = br#"{"jsonrpc":"2.0","method":"exit"}"#;
        assert!(is_exit_candidate(body));
        assert!(is_exit_notification(body));
    }

    /// A whole-document `didChange` body without the token `exit` is rejected by the
    /// cheap substring pre-guard, so it never reaches the full `serde_json` parse on the
    /// transport hot path — the frames that dominate a session skip the parse.
    #[test]
    fn a_large_did_change_body_without_the_token_skips_the_json_parse() {
        let mut body =
            br#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":{"text":""#.to_vec();
        body.extend_from_slice(&vec![b'x'; 4096]);
        body.extend_from_slice(br#""}}"#);
        assert!(
            !is_exit_candidate(&body),
            "a body without the token must be rejected before the JSON parse"
        );
        assert!(!is_exit_notification(&body));
    }

    /// A conformant `exit` notification larger than any tight size cap — pretty-printed
    /// or padded with insignificant whitespace, as a verbose client's serializer can
    /// produce — must still be recognized. Capping detection on length would
    /// false-negate it, so the server would never call `exit_now` and would hang for a
    /// client that holds stdin open.
    #[test]
    fn a_large_exit_notification_is_still_recognized() {
        let mut body = br#"{ "jsonrpc": "2.0", "method": "exit", "params": null"#.to_vec();
        // Whitespace inside the object is insignificant JSON but pushes the frame well
        // past the old 128-byte cap.
        body.extend_from_slice(&[b' '; 128]);
        body.push(b'}');
        assert!(
            body.len() > 128,
            "the frame must exceed the old cap to matter"
        );
        assert!(is_exit_candidate(&body));
        assert!(is_exit_notification(&body));
    }

    /// A small frame whose method merely contains `exit` passes the pre-guard but the
    /// full parse rejects it, so the token guard never over-accepts.
    #[test]
    fn a_small_non_exit_frame_is_rejected_by_the_parse() {
        let body = br#"{"jsonrpc":"2.0","method":"exithook"}"#;
        assert!(is_exit_candidate(body));
        assert!(!is_exit_notification(body));
    }
}
