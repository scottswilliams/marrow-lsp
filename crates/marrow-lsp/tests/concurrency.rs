//! Concurrency and cancellation regression net for the LSP transport.
//!
//! Drives the real `marrow-lsp` server over stdio with interleaved `didChange`,
//! `hover`, `completion`, and `$/cancelRequest` frames, asserting the transport never
//! deadlocks and that cancelling an in-flight request does not corrupt a later one. A
//! reader thread demultiplexes messages so the assertions correlate on request ids —
//! the protocol — rather than on wall-clock timing. The bounded receive only fires on a
//! genuine hang; a healthy server answers in milliseconds.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// A true deadlock, not a slow machine, is the only thing this bound catches: a
/// responsive server answers each request in milliseconds.
const NO_DEADLOCK_BOUND: Duration = Duration::from_secs(20);

fn send(stdin: &mut impl Write, message: &Value) {
    let body = serde_json::to_string(message).unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    stdin.flush().unwrap();
}

fn recv_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>().ok()?);
        }
    }
    let length = content_length?;
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn the built server and a reader thread that forwards every framed message to a
/// channel, so responses and notifications can arrive in any order and still be
/// demultiplexed by the test.
fn spawn_server() -> (Server, ChildStdin, mpsc::Receiver<Value>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_marrow-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the marrow-lsp binary runs");
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        while let Some(message) = recv_message(&mut stdout) {
            if tx.send(message).is_err() {
                break;
            }
        }
    });
    (Server(child), stdin, rx)
}

/// Read messages until a response for every id in `ids` has arrived, returning them
/// keyed by id. Notifications and unrelated responses are drained past. Fails on a
/// genuine hang rather than blocking a stuck run forever.
fn responses_for(rx: &mpsc::Receiver<Value>, ids: &[i64]) -> HashMap<i64, Value> {
    let mut wanted: Vec<i64> = ids.to_vec();
    let mut collected = HashMap::new();
    let deadline = Instant::now() + NO_DEADLOCK_BOUND;
    while !wanted.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .expect("the server deadlocked: no response before the bound");
        let message = rx
            .recv_timeout(remaining)
            .expect("the server deadlocked: the response stream stalled");
        if let Some(id) = message.get("id").and_then(Value::as_i64)
            && let Some(position) = wanted.iter().position(|candidate| *candidate == id)
        {
            wanted.swap_remove(position);
            collected.insert(id, message);
        }
    }
    collected
}

fn initialize(stdin: &mut ChildStdin, rx: &mpsc::Receiver<Value>) {
    send(
        stdin,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "capabilities": {} } }),
    );
    responses_for(rx, &[1]);
    send(
        stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );
}

/// A minimal memory-store project so the server resolves a project from the opened
/// file's path and can answer hover/completion.
fn write_project(dir: &std::path::Path) -> String {
    std::fs::write(
        dir.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("app.mw");
    let source = "module app\n\npub fn answer(): int\n    return 42\n\n\
                  pub fn caller(): int\n    return answer()\n";
    std::fs::write(&file, source).unwrap();
    url::Url::from_file_path(&file).unwrap().to_string()
}

/// Interleaving didChange with two reads and a cancellation must neither wedge the
/// transport nor let the cancelled request corrupt a later one: both surviving reads
/// answer, and a fresh hover afterwards still returns a well-formed response.
///
/// Whether the `$/cancelRequest` for id 10 lands while that hover is genuinely in
/// flight or after it has already resolved is racy — driving the real server over
/// stdio, the test cannot park the hover without adding production-only test surface,
/// which is not worth it. The assertions are non-vacuous under either ordering: the
/// transport must stay deadlock-free and every surviving request must answer cleanly
/// regardless of when the cancel is observed.
#[test]
fn interleaved_reads_and_a_cancel_do_not_deadlock_or_corrupt_a_later_request() {
    let dir = tempfile::tempdir().unwrap();
    let uri = write_project(dir.path());
    let (_server, mut stdin, rx) = spawn_server();
    initialize(&mut stdin, &rx);

    let source = "module app\n\npub fn answer(): int\n    return 42\n\n\
                  pub fn caller(): int\n    return answer()\n";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": { "textDocument": { "uri": uri, "languageId": "marrow", "version": 1, "text": source } }
        }),
    );

    // A burst: edit the buffer, request hover and completion against it, cancel the
    // hover mid-flight, then edit and request hover again — all before draining.
    let answer_call = json!({ "line": 6, "character": 11 });
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": format!("{source}\npub fn added(): int\n    return 1\n") }]
            }
        }),
    );
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 10, "method": "textDocument/hover",
                 "params": { "textDocument": { "uri": uri }, "position": answer_call } }),
    );
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 11, "method": "textDocument/completion",
                 "params": { "textDocument": { "uri": uri }, "position": answer_call } }),
    );
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "$/cancelRequest", "params": { "id": 10 } }),
    );
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 12, "method": "textDocument/hover",
                 "params": { "textDocument": { "uri": uri }, "position": answer_call } }),
    );

    // Every request is answered (no deadlock). The cancelled hover resolves either as a
    // cancellation error or a normal result — the transport handled it either way.
    let answered = responses_for(&rx, &[10, 11, 12]);

    let completion = &answered[&11];
    assert!(
        completion.get("error").is_none() && completion["result"].is_array(),
        "completion must answer with an item array while requests interleave: {completion}"
    );

    let later_hover = &answered[&12];
    assert!(
        later_hover.get("error").is_none(),
        "a hover issued after the cancellation must not inherit the cancelled request's error: {later_hover}"
    );
    assert!(
        later_hover.get("result").is_some(),
        "the later hover must carry a result field (a value or null), proving the server stayed healthy: {later_hover}"
    );

    // A final standalone request confirms the session is still live and correct after
    // the interleaved burst.
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 13, "method": "textDocument/completion",
                 "params": { "textDocument": { "uri": uri }, "position": answer_call } }),
    );
    let final_completion = responses_for(&rx, &[13]);
    assert!(
        final_completion[&13]["result"].is_array(),
        "the server must keep answering after a cancelled request: {:?}",
        final_completion[&13]
    );
}
