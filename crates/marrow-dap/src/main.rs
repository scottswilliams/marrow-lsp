//! The Marrow DAP server: a statement-level debugger over stdio.
//!
//! All language intelligence lives in `marrow-lsp-core` and the canonical marrow
//! crates; this binary is the DAP transport over them — the third transport beside
//! the LSP and MCP servers. It speaks `Content-Length`-framed JSON on stdin/stdout
//! (the DAP/LSP wire family) and drives the runtime's opt-in statement hook to step
//! and inspect a real `.mw` run.
//!
//! ## Why threads (the crux)
//! The interpreter is single-threaded and synchronous, and the debugger's
//! [`Frame`](marrow_run::Frame) borrows the live environment, so it cannot cross a
//! thread boundary. The run executes on a dedicated *run-thread*; this protocol
//! loop runs on the main thread. They meet in the run's `before_statement` hook:
//! at a stop, the run-thread parks (still holding the frame) and serves owned
//! answers to the protocol thread's queries over channels. The live frame stays
//! on the run-thread; see [`debugger`] for the full rendezvous.

mod debugger;
mod project;
mod protocol;
mod run;
mod session;
mod step;
mod variables;

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use marrow_terminal::{Style, paint_stderr};
use session::Session;

fn stderr_notice(label: &str) -> String {
    paint_stderr(Style::Notice, label)
}

fn main() {
    // Startup is observable in the editor's debug-adapter output channel; protocol
    // messages go to stdout, so this note goes to stderr.
    eprintln!(
        "{} starting (version {})",
        stderr_notice("marrow-dap:"),
        env!("CARGO_PKG_VERSION")
    );
    let stdout = std::io::stdout();
    let input = BufReader::new(std::io::stdin());
    let out = stdout.lock();
    serve(input, out);
}

/// The protocol loop dispatches client requests and run-thread events until the
/// client disconnects or the session ends. Requests already accepted into the
/// internal queue are handled before staged resumes and newly available run
/// events, so those requests observe the resumed state before the run is woken.
fn serve<R>(mut input: R, out: impl Write)
where
    R: BufRead + Send + 'static,
{
    let (requests_tx, requests_rx) = mpsc::channel();
    std::thread::spawn(move || {
        while let Some(message) = protocol::read_message(&mut input) {
            if message.get("type").and_then(|value| value.as_str()) == Some("request")
                && requests_tx.send(message).is_err()
            {
                break;
            }
        }
    });

    let mut session = Session::new(out);
    loop {
        while let Ok(message) = requests_rx.try_recv() {
            session.handle(&message);
            if session.done() {
                return;
            }
        }

        if session.flush_resume() {
            if session.done() {
                return;
            }
            continue;
        }

        if session.pump_run_event() {
            if session.done() {
                return;
            }
            continue;
        }

        match requests_rx.recv_timeout(Duration::from_millis(5)) {
            Ok(message) => session.handle(&message),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if session.done() {
            return;
        }
    }
}
