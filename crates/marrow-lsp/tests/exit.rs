//! The LSP `exit` notification must terminate the server process.
//!
//! Per the LSP spec, `shutdown` (a request) is followed by `exit` (a
//! notification), and on `exit` the server exits immediately — code 0 when a
//! prior `shutdown` was honored. The process must NOT depend on stdin closing:
//! an editor keeps the pipe open and expects the server to exit on the
//! notification alone. This drives the real binary and asserts it does.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Frame a JSON-RPC message with the LSP `Content-Length` header and write it.
fn send(stdin: &mut impl Write, message: &Value) {
    let body = serde_json::to_string(message).unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    stdin.flush().unwrap();
}

/// Read one framed JSON-RPC message: the `Content-Length` header then the body.
fn recv(reader: &mut impl BufRead) -> Value {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let length = content_length.expect("a Content-Length header");
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

/// Block until `child` exits or the deadline passes, returning its exit code.
/// Polls rather than blocking so a hung server fails the test instead of it.
fn wait_for_exit(child: &mut std::process::Child, timeout: Duration) -> Option<i32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("polling the child") {
            return status.code();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// initialize → shutdown (response) → exit (notification): the server must exit
/// with code 0 within a couple of seconds, while its stdin stays open. This is
/// the editor's real teardown sequence; a hang here strands the process.
#[test]
fn exit_notification_terminates_the_process_with_stdin_open() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_marrow-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the marrow-lsp binary runs");
    // Hold stdin open for the whole sequence: the exit must not need EOF.
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "capabilities": {} } }),
    );
    let response = recv(&mut stdout);
    assert_eq!(response["result"]["serverInfo"]["name"], "marrow-lsp");

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown" }),
    );
    let response = recv(&mut stdout);
    assert_eq!(response["id"], 2, "shutdown is answered");
    assert!(response.get("error").is_none(), "shutdown succeeds");

    send(&mut stdin, &json!({ "jsonrpc": "2.0", "method": "exit" }));

    let code = wait_for_exit(&mut child, Duration::from_secs(2));
    // Keep stdin alive until after the assertion so the exit cannot be attributed
    // to a closed pipe.
    drop(stdin);
    assert_eq!(
        code,
        Some(0),
        "the server must exit with code 0 on the exit notification (it hung or exited non-zero)"
    );
}

/// The pre-existing teardown — closing stdin (EOF) — must still terminate the
/// server, so the exit handling did not regress the stream-closed path.
#[test]
fn closing_stdin_still_terminates_the_process() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_marrow-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the marrow-lsp binary runs");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "capabilities": {} } }),
    );
    let _ = recv(&mut stdout);

    // Drop stdin: the client closed the pipe without an exit notification.
    drop(stdin);

    let code = wait_for_exit(&mut child, Duration::from_secs(5));
    assert!(
        code.is_some(),
        "the server must terminate when stdin closes (EOF)"
    );
}
