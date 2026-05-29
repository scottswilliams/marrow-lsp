//! End-to-end smoke test: drive the built server over stdio with real LSP framing,
//! initialize it, open an erroring `.mw` buffer in a real project, and assert the
//! published diagnostic. This proves the transport, the project resolution, and the
//! overlay-driven recompute work together, not just that the crate builds.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Frame a JSON-RPC message with the LSP `Content-Length` header and write it.
fn send(stdin: &mut impl Write, message: &Value) {
    let body = serde_json::to_string(message).unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    stdin.flush().unwrap();
}

/// Read one framed JSON-RPC message: parse the `Content-Length` header, then read
/// exactly that many body bytes.
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

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/shelf")
        .canonicalize()
        .expect("the shelf fixture exists")
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn initialize_then_open_erroring_buffer_publishes_a_diagnostic() {
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_marrow-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the marrow-lsp binary runs"),
    );
    let mut stdin = server.0.stdin.take().unwrap();
    let mut stdout = BufReader::new(server.0.stdout.take().unwrap());

    // initialize → expect the server's name back, advertising full-text sync.
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": {} }
        }),
    );
    let response = recv(&mut stdout);
    assert_eq!(
        response["result"]["serverInfo"]["name"], "marrow-lsp",
        "initialize should report the server name"
    );
    assert_eq!(
        response["result"]["capabilities"]["textDocumentSync"], 1,
        "full-text sync (1) should be advertised"
    );

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    // Open the fixture's module file with an overlay that returns a string from an
    // int-returning function: a real check.return_type error against a real project.
    let file = fixture_root().join("src/shelf/sample.mw");
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    let erroring = "module shelf::sample\n\npub fn answer(): int\n    return \"nope\"\n";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "marrow",
                    "version": 1,
                    "text": erroring
                }
            }
        }),
    );

    // Wait for the publishDiagnostics notification carrying our error.
    let diagnostic = wait_for_diagnostic(&mut stdout, &uri, Duration::from_secs(10));
    assert_eq!(
        diagnostic["code"], "check.return_type",
        "the overlay's return-type error should be published"
    );
    assert_eq!(diagnostic["source"], "marrow");

    let _ = server.0.kill();
}

/// Read notifications until a non-empty `publishDiagnostics` for `uri` arrives,
/// returning its first diagnostic. Recomputes are debounced, so an initial empty
/// publish for other open files may precede it.
fn wait_for_diagnostic(reader: &mut impl BufRead, uri: &str, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let message = recv(reader);
        if message["method"] == "textDocument/publishDiagnostics" {
            let params = &message["params"];
            let diagnostics = params["diagnostics"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if params["uri"] == uri && !diagnostics.is_empty() {
                return diagnostics[0].clone();
            }
        }
    }
    panic!("no diagnostic published within the timeout");
}
