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
    assert_eq!(
        response["result"]["capabilities"]["signatureHelpProvider"]["triggerCharacters"],
        json!(["(", ","]),
        "signature help should trigger while starting and continuing calls"
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

#[test]
fn signature_help_returns_null_without_a_checked_program() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("scratch.mw");
    let source = "module scratch\n\npub fn run(): int\n    return int(";
    std::fs::write(&file, source).unwrap();

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

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": {} }
        }),
    );
    let _ = recv(&mut stdout);
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    let uri = url::Url::from_file_path(&file).unwrap().to_string();
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
                    "text": source
                }
            }
        }),
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/signatureHelp",
            "params": {
                "textDocument": { "uri": uri },
                "position": {
                    "line": 3,
                    "character": source.lines().last().unwrap().len()
                }
            }
        }),
    );

    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert!(
        response["result"].is_null(),
        "signature help outside a checked project should be null, got {response:?}"
    );

    let _ = server.0.kill();
}

#[test]
fn signature_help_returns_null_for_scratch_file_after_project_program_exists() {
    let scratch_dir = tempfile::tempdir().unwrap();
    let scratch_file = scratch_dir.path().join("scratch.mw");
    let scratch_source = "module scratch\n\npub fn run(): int\n    return int(";
    std::fs::write(&scratch_file, scratch_source).unwrap();

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

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": {} }
        }),
    );
    let _ = recv(&mut stdout);
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    let project_file = fixture_root().join("src/shelf/sample.mw");
    let project_uri = url::Url::from_file_path(&project_file).unwrap().to_string();
    let clean = "module shelf::sample\n\npub fn answer(): int\n    return 1\n";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": project_uri,
                    "languageId": "marrow",
                    "version": 1,
                    "text": clean
                }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &project_uri, Duration::from_secs(10));

    let scratch_uri = url::Url::from_file_path(&scratch_file).unwrap().to_string();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": scratch_uri,
                    "languageId": "marrow",
                    "version": 1,
                    "text": scratch_source
                }
            }
        }),
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/signatureHelp",
            "params": {
                "textDocument": { "uri": scratch_uri },
                "position": {
                    "line": 3,
                    "character": scratch_source.lines().last().unwrap().len()
                }
            }
        }),
    );

    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert!(
        response["result"].is_null(),
        "signature help for a scratch file outside the checked program should be null, got {response:?}"
    );

    let _ = server.0.kill();
}

#[test]
fn hover_over_a_typed_expression_returns_a_marrow_code_block() {
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
        response["result"]["capabilities"]["hoverProvider"], true,
        "the server should advertise hover support"
    );
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    // A clean buffer for the fixture's module path: a function whose parameter `n`
    // is an int, used on the last line.
    let file = fixture_root().join("src/shelf/sample.mw");
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    let clean = "module shelf::sample\n\npub fn answer(n: int): int\n    return n\n";
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
                    "text": clean
                }
            }
        }),
    );

    // Hover over the `n` in `return n` on line 3 (zero-based), character 11.
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 3, "character": 11 }
            }
        }),
    );

    let hover = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert_eq!(
        hover["result"]["contents"]["value"], "```marrow\nint\n```",
        "hovering the int parameter should render its type as a marrow block"
    );

    let _ = server.0.kill();
}

#[test]
fn goto_definition_jumps_from_a_use_to_its_binding() {
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
        response["result"]["capabilities"]["definitionProvider"], true,
        "the server should advertise go-to-definition"
    );
    assert_eq!(
        response["result"]["capabilities"]["referencesProvider"], true,
        "the server should advertise find-references"
    );
    assert_eq!(
        response["result"]["capabilities"]["renameProvider"]["prepareProvider"], true,
        "the server should advertise prepare-rename"
    );
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    // A buffer binding a local `n` on line 3 (zero-based) and using it on line 4.
    let file = fixture_root().join("src/shelf/sample.mw");
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    let clean = "module shelf::sample\n\npub fn f(): int\n    const n: int = 1\n    return n\n";
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
                    "text": clean
                }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &uri, Duration::from_secs(10));

    // Go to definition from the `n` in `return n` (line 4, after `    return `).
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 4, "character": 11 }
            }
        }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    let location = &response["result"];
    assert_eq!(location["uri"], uri, "definition is in the same file");
    // The binding is the `n` in `const n` on line 3, at `    const ` = 10 columns.
    assert_eq!(
        location["range"]["start"]["line"], 3,
        "definition lands on the const binding line"
    );
    assert_eq!(location["range"]["start"]["character"], 10);

    let _ = server.0.kill();
}

#[test]
fn completion_after_caret_lists_saved_roots() {
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
        response["result"]["capabilities"]["completionProvider"]["triggerCharacters"][0], "^",
        "the server should advertise `^` as a completion trigger character"
    );
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    // Open a clean buffer declaring the saved resource `Book at ^books`, so the
    // analysis caches a program carrying that resource.
    let file = fixture_root().join("src/shelf/sample.mw");
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    let clean = "module shelf::sample\n\nresource Book at ^books(id: int)\n    required title: string\n\npub fn drop()\n    return\n";
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
                    "text": clean
                }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &uri, Duration::from_secs(10));

    // Now the user types a `delete ^` — the file no longer parses cleanly, which
    // drops its module from a fresh analysis. Completion must still list the saved
    // root, drawn from the last clean program.
    let erroring = "module shelf::sample\n\nresource Book at ^books(id: int)\n    required title: string\n\npub fn drop()\n    delete ^\n";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [ { "text": erroring } ]
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &uri, Duration::from_secs(10));

    // The `^` is the last non-newline character: line 6 (zero-based), after `delete `.
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 6, "character": 12 }
            }
        }),
    );

    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    let labels: Vec<String> = response["result"]
        .as_array()
        .expect("completion returns an array of items")
        .iter()
        .map(|item| item["label"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        labels.contains(&"books".to_string()),
        "completion after `^` should list the saved root `books`, got {labels:?}"
    );

    let _ = server.0.kill();
}

#[test]
fn code_lens_reports_a_record_count_for_a_saved_resource() {
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
        response["result"]["capabilities"]["codeLensProvider"]["resolveProvider"], true,
        "the server should advertise a resolving code-lens provider"
    );
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    // Open the fixture's module, which declares `resource Book at ^books`.
    let file = fixture_root().join("src/shelf/sample.mw");
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    let text = std::fs::read_to_string(&file).unwrap();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": { "uri": uri, "languageId": "marrow", "version": 1, "text": text }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &uri, Duration::from_secs(10));

    // Request code lenses: one unresolved lens for the saved `Book` resource.
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/codeLens",
            "params": { "textDocument": { "uri": uri } }
        }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    let lenses = response["result"].as_array().expect("an array of lenses");
    assert_eq!(lenses.len(), 1, "one saved resource, one lens");
    let lens = lenses[0].clone();
    assert_eq!(lens["data"]["root"], "books");
    assert!(lens["command"].is_null(), "the lens is unresolved");

    // Resolve it. The fixture has no store file, so the store is unavailable and
    // the lens stays quiet (no command) — the soft-degrade path, not an error.
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "codeLens/resolve",
            "params": lens
        }),
    );
    let resolved = wait_for_response(&mut stdout, 3, Duration::from_secs(10));
    assert!(
        resolved["result"]["command"].is_null(),
        "an unavailable store resolves to a quiet lens, got {:?}",
        resolved["result"]
    );

    let _ = server.0.kill();
}

#[test]
fn custom_saved_roots_request_answers_over_the_transport() {
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

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "capabilities": {} }
        }),
    );
    let _ = recv(&mut stdout);
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    // Open the fixture so a project (with its native store config) is resolved.
    let file = fixture_root().join("src/shelf/sample.mw");
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    let text = std::fs::read_to_string(&file).unwrap();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": { "uri": uri, "languageId": "marrow", "version": 1, "text": text }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &uri, Duration::from_secs(10));

    // The custom Data Explorer request answers with a well-formed envelope. The
    // fixture has no store file, so the store is unavailable: `available` is false
    // and `roots` is empty — a soft degrade, never a JSON-RPC error.
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "marrow/savedRoots" }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert!(
        response.get("error").is_none(),
        "a custom request must not error, got {response:?}"
    );
    assert_eq!(response["result"]["available"], false);
    assert_eq!(
        response["result"]["roots"].as_array().map(Vec::len),
        Some(0)
    );

    let _ = server.0.kill();
}

#[test]
fn custom_saved_get_returns_a_typed_value_from_a_seeded_store() {
    use marrow_store::backend::Backend;
    use marrow_store::path::{PathSegment, SavedKey, encode_path};
    use marrow_store::redb::RedbStore;

    // A temp project pinning a native store, seeded with `^books(1).title="Mort"`.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    let source = "module shelf\n\nresource Book at ^books(id: int)\n    required title: string\n\npub fn f()\n    return\n";
    let file = src.join("shelf.mw");
    std::fs::write(&file, source).unwrap();
    {
        let mut store = RedbStore::open(&root.join("data").join("marrow.redb")).unwrap();
        let title = encode_path(&[
            PathSegment::Root("books".to_string()),
            PathSegment::RecordKey(SavedKey::Int(1)),
            PathSegment::Field("title".to_string()),
        ]);
        store.write(&title, b"Mort".to_vec()).unwrap();
    }

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

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "capabilities": {} } }),
    );
    let _ = recv(&mut stdout);
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": { "uri": uri, "languageId": "marrow", "version": 1, "text": source }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &uri, Duration::from_secs(10));

    // Read `^books(1).title` through the custom request: a typed `string` value.
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "marrow/savedGet",
            "params": {
                "path": [
                    { "root": "books" },
                    { "key": { "int": 1 } },
                    { "field": "title" }
                ]
            }
        }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert!(response.get("error").is_none(), "no error: {response:?}");
    let result = &response["result"];
    assert_eq!(result["available"], true, "the seeded store is readable");
    assert_eq!(result["presence"], "value_only");
    assert_eq!(result["value"], "\"Mort\"");
    assert_eq!(result["type"], "string");

    let _ = server.0.kill();
}

#[test]
fn custom_data_integrity_flags_an_orphan_from_a_seeded_store() {
    use marrow_store::backend::Backend;
    use marrow_store::path::{PathSegment, SavedKey, encode_path};
    use marrow_store::redb::RedbStore;

    // A temp project pinning a native store. The schema declares `^books(id).title`;
    // the store also holds `^books(1).sticker`, a field the schema does not declare,
    // so the schema-impact scan must flag it as an orphan.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    let source = "module shelf\n\nresource Book at ^books(id: int)\n    required title: string\n\npub fn f()\n    return\n";
    let file = src.join("shelf.mw");
    std::fs::write(&file, source).unwrap();
    {
        let mut store = RedbStore::open(&root.join("data").join("marrow.redb")).unwrap();
        let title = encode_path(&[
            PathSegment::Root("books".to_string()),
            PathSegment::RecordKey(SavedKey::Int(1)),
            PathSegment::Field("title".to_string()),
        ]);
        store.write(&title, b"Mort".to_vec()).unwrap();
        let orphan = encode_path(&[
            PathSegment::Root("books".to_string()),
            PathSegment::RecordKey(SavedKey::Int(1)),
            PathSegment::Field("sticker".to_string()),
        ]);
        store.write(&orphan, b"gold".to_vec()).unwrap();
    }

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

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "capabilities": {} } }),
    );
    let _ = recv(&mut stdout);
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": { "uri": uri, "languageId": "marrow", "version": 1, "text": source }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &uri, Duration::from_secs(10));

    // The on-demand schema-impact advisory over the transport.
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "marrow/dataIntegrity", "params": {} }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert!(response.get("error").is_none(), "no error: {response:?}");
    let result = &response["result"];
    assert_eq!(result["available"], true, "the seeded store is readable");
    assert_eq!(result["scanned"], 2, "two stored entries scanned: {result}");
    assert_eq!(result["truncated"], false);
    let findings = result["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "one orphan finding: {result}");
    assert_eq!(findings[0]["kind"], "orphan");
    assert_eq!(findings[0]["path"], "^books(1).sticker");

    let _ = server.0.kill();
}

/// Read notifications until a `publishDiagnostics` for `uri` arrives (empty or
/// not), returning it. Used to wait for the debounced recompute to land before a
/// request that depends on the fresh snapshot.
fn wait_for_diagnostic_or_empty(reader: &mut impl BufRead, uri: &str, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let message = recv(reader);
        if message["method"] == "textDocument/publishDiagnostics" && message["params"]["uri"] == uri
        {
            return message;
        }
    }
    panic!("no diagnostics published within the timeout");
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

/// Read messages until the response with `id` arrives, skipping the interleaved
/// diagnostic notifications a recompute publishes.
fn wait_for_response(reader: &mut impl BufRead, id: i64, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let message = recv(reader);
        if message["id"] == id {
            return message;
        }
    }
    panic!("no response for request {id} within the timeout");
}
