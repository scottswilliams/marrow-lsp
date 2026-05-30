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
//! answers to the protocol thread's queries over channels. No `unsafe`, no
//! `Send` of a frame — see [`debugger`] for the full rendezvous.

mod breakpoints;
mod debugger;
mod durable;
mod project;
mod protocol;
mod run;
mod session;
mod step;
mod variables;

use std::io::{BufReader, Write};

use session::Session;

fn main() {
    // Startup is observable in the editor's debug-adapter output channel; protocol
    // messages go to stdout, so this note goes to stderr.
    eprintln!(
        "marrow-dap: starting (version {})",
        env!("CARGO_PKG_VERSION")
    );
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = BufReader::new(stdin.lock());
    let out = stdout.lock();
    serve(&mut input, out);
}

/// The protocol loop: read one framed request at a time and dispatch it to the
/// session until the client disconnects or the stream closes. The session pumps
/// run-thread events inline (a request that resumes the run blocks here until the
/// next stop or the run's end), so all client-visible ordering is sequential.
fn serve(input: &mut impl std::io::BufRead, out: impl Write) {
    let mut session = Session::new(out);
    while let Some(message) = protocol::read_message(input) {
        // DAP carries requests as `type: "request"`; ignore anything else.
        if message.get("type").and_then(|value| value.as_str()) == Some("request") {
            session.handle(&message);
        }
        if session.done() {
            break;
        }
    }
}
