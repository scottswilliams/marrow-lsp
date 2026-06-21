//! End-to-end smoke test: drive the built server over stdio with real LSP framing,
//! initialize it, open an erroring `.mw` buffer in a real project, and assert the
//! published diagnostic. This proves the transport, the project resolution, and the
//! overlay-driven recompute work together, not just that the crate builds.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use marrow_check::test_support::{member_catalog_id, root_place, store_id_of};
use marrow_check::tooling::{DataChild, DataPathSegment as ToolingDataPathSegment, data_children};
use marrow_store::cell::CatalogId;
use marrow_store::key::SavedKey;
use marrow_store::tree::{DataPathSegment, StoreUid, TreeStore};
use marrow_store::value::{SavedValue, encode_value};
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

fn native_counter_fixture() -> (tempfile::TempDir, PathBuf) {
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
    let source = "\
module counter

resource Counter
    required value: int

store ^counter(id: int): Counter

pub fn f()
    return
";
    let file = src.join("counter.mw");
    std::fs::write(&file, source).unwrap();

    let config_text = std::fs::read_to_string(root.join("marrow.json")).unwrap();
    let config = marrow_project::parse_config(&config_text).unwrap();
    let (report, proposed) = marrow_check::check_project(root, &config).unwrap();
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let store = TreeStore::open(&root.join("data").join("marrow.redb")).unwrap();
    store
        .write_store_uid(&StoreUid::new("store_00000000000000000000000000000001").unwrap())
        .unwrap();
    marrow_run::evolution::commit_catalog_baseline(&store, &proposed).unwrap();
    let accepted = store.read_catalog_snapshot().unwrap();
    let (report, program) =
        marrow_check::check_project_with_catalog(root, &config, accepted.as_ref()).unwrap();
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    if let Some(snapshot) = &accepted {
        marrow_check::project_store_lock(root, snapshot, &program.source_digest()).unwrap();
    }
    let place = root_place(&program, "counter").unwrap();
    let store_id = store_id_of(&place).unwrap();
    let value_id = CatalogId::new(member_catalog_id(&place, "value").unwrap()).unwrap();
    let identity = [SavedKey::Int(1)];
    store.write_record_presence(&store_id, &identity).unwrap();
    store
        .write_data_value(
            &store_id,
            &identity,
            &[DataPathSegment::Member(value_id)],
            encode_value(&SavedValue::Int(42)).unwrap(),
        )
        .unwrap();
    let page = data_children(
        &program,
        &store,
        &[ToolingDataPathSegment::Root("counter".into())],
        10,
        None,
    )
    .unwrap();
    assert_eq!(page.children, vec![DataChild::Key(SavedKey::Int(1))]);

    (dir, file)
}

fn assert_store_snapshot(snapshot: &Value) {
    assert_eq!(
        snapshot["profile_version"], "data.generation.v1",
        "snapshot should carry the Marrow data generation profile version: {snapshot}"
    );
    assert_eq!(
        snapshot["store_uid"], "store_00000000000000000000000000000001",
        "snapshot should carry the native fixture store UID: {snapshot}"
    );
    assert!(
        snapshot["catalog_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "snapshot should carry a catalog digest: {snapshot}"
    );
    let commit = &snapshot["commit"];
    assert!(
        commit["commit_id"].is_u64(),
        "snapshot should carry commit metadata: {snapshot}"
    );
    assert!(
        commit["catalog_epoch"].is_u64(),
        "snapshot commit should carry a catalog epoch: {snapshot}"
    );
    assert!(
        commit["source_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "snapshot commit should carry a source digest: {snapshot}"
    );
    assert!(
        commit["layout_epoch"].is_u64(),
        "snapshot commit should carry a layout epoch: {snapshot}"
    );
    assert!(
        commit["engine_profile_digest"]
            .as_str()
            .is_some_and(|digest| {
                digest.len() == 16
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            }),
        "snapshot commit should carry canonical profile digest hex: {snapshot}"
    );
    assert!(
        snapshot["checked_source_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "snapshot should carry the checked program source digest: {snapshot}"
    );
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn initialized_server() -> (Server, ChildStdin, BufReader<ChildStdout>, Value) {
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
    let initialize = recv(&mut stdout);
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );
    (server, stdin, stdout, initialize)
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
    let lens_provider_key = ["code", "Lens", "Provider"].concat();
    assert!(
        response["result"]["capabilities"][&lens_provider_key].is_null(),
        "lens support must stay unadvertised until Marrow exposes a canonical per-root count fact: {response}"
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
fn opening_shelf_fixture_publishes_no_diagnostics() {
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

    let diagnostics = wait_for_diagnostic_or_empty(&mut stdout, &uri, Duration::from_secs(10));
    assert_eq!(
        diagnostics["params"]["diagnostics"],
        json!([]),
        "the checked shelf fixture should publish no diagnostics"
    );

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
fn hover_over_a_parameter_use_returns_its_signature_fragment() {
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
        hover["result"]["contents"]["value"], "```marrow\nn: int\n```",
        "hovering the int parameter use should render its parameter signature"
    );

    let _ = server.0.kill();
}

#[test]
fn hover_over_function_call_reports_checked_direct_effects_over_stdio() {
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

    let file = fixture_root().join("src/shelf/sample.mw");
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    let clean = "\
module shelf::sample

resource Book
    required title: string
    required visits: int

store ^books(id: int): Book

pub fn touch(id: int): string
    const title: string = ^books(id).title ?? \"\"
    const visits: int = ^books(id).visits ?? 0
    transaction
        ^books(id).visits = visits + 1
    print(title)
    return title

pub fn caller(): string
    return touch(1)
";
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
    let diagnostics = wait_for_diagnostic_or_empty(&mut stdout, &uri, Duration::from_secs(10));
    assert_eq!(
        diagnostics["params"]["diagnostics"],
        json!([]),
        "the clean direct-effect overlay should check without diagnostics"
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/hover",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 17, "character": 12 }
            }
        }),
    );

    let hover = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    let value = hover["result"]["contents"]["value"]
        .as_str()
        .expect("hover should return markdown contents");
    assert!(
        value.contains("fn touch(id: int): string"),
        "function signature should be present in hover markdown: {value}"
    );
    assert!(
        value.contains("**Direct effects**"),
        "function hover should include a direct-effects section: {value}"
    );
    assert!(
        value.contains("- saved reads: Book.title, Book.visits"),
        "direct saved reads should survive stdio transport: {value}"
    );
    assert!(
        value.contains("- saved writes: Book.visits"),
        "direct saved writes should survive stdio transport: {value}"
    );
    assert!(
        value.contains("- transaction"),
        "direct transaction effect should survive stdio transport: {value}"
    );
    assert!(
        value.contains("- host: output"),
        "direct host output effect should survive stdio transport: {value}"
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
fn definition_returns_null_when_open_buffer_is_newer_than_cached_snapshot() {
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

    let edited = "module shelf::sample\n\npub fn f(): int\n    const m: int = 1\n    return 1\n";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [ { "text": edited } ]
            }
        }),
    );

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
    assert_eq!(
        response["result"],
        Value::Null,
        "definition must not return facts from the pre-edit snapshot: {response}"
    );

    let _ = server.0.kill();
}

#[test]
fn definition_returns_null_when_open_source_overlay_closes_before_recompute() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let defs_file = src.join("defs.mw");
    let app_file = src.join("app.mw");
    std::fs::write(
        &defs_file,
        "module defs\n\npub fn answer(): int\n    return 1\n",
    )
    .unwrap();
    std::fs::write(
        &app_file,
        "module app\nuse defs\n\npub fn call(): int\n    return defs::answer()\n",
    )
    .unwrap();

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

    let defs_uri = url::Url::from_file_path(&defs_file).unwrap().to_string();
    let defs_overlay = "module defs\n\npub fn renamed(): int\n    return 1\n";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": defs_uri,
                    "languageId": "marrow",
                    "version": 1,
                    "text": defs_overlay
                }
            }
        }),
    );
    let app_uri = url::Url::from_file_path(&app_file).unwrap().to_string();
    let app_overlay = "\
module app
use defs

pub fn call(): int
    return defs::renamed()
";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": app_uri,
                    "languageId": "marrow",
                    "version": 1,
                    "text": app_overlay
                }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &app_uri, Duration::from_secs(10));

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": app_uri },
                "position": { "line": 4, "character": "    return defs::".len() }
            }
        }),
    );
    let before_close = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert_eq!(
        before_close["result"]["uri"], defs_uri,
        "the overlay function should be resolvable before the overlay closes"
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didClose",
            "params": {
                "textDocument": { "uri": defs_uri }
            }
        }),
    );
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": app_uri },
                "position": { "line": 4, "character": "    return defs::".len() }
            }
        }),
    );
    let after_close = wait_for_response(&mut stdout, 3, Duration::from_secs(10));
    assert_eq!(
        after_close["result"],
        Value::Null,
        "closed overlay contributors must invalidate binding facts before the debounced recompute: {after_close}"
    );

    let _ = server.0.kill();
}

#[test]
fn symbols_do_not_return_stale_snapshot_names_after_an_edit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("app.mw");
    let clean = "module app\n\npub fn old_name(): int\n    return 1\n";
    std::fs::write(&file, clean).unwrap();

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
                    "text": clean
                }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &uri, Duration::from_secs(10));

    let edited = "module app\n\npub fn new_name(): int\n    return 1\n";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [ { "text": edited } ]
            }
        }),
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/documentSymbol",
            "params": { "textDocument": { "uri": uri } }
        }),
    );
    let document_symbols = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    let names: Vec<&str> = document_symbols["result"]
        .as_array()
        .expect("document symbols result")
        .iter()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    assert!(
        names.contains(&"new_name"),
        "document symbols should read the open parser, got {document_symbols}"
    );
    assert!(
        !names.contains(&"old_name"),
        "document symbols must not expose stale snapshot names, got {document_symbols}"
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "workspace/symbol",
            "params": { "query": "name" }
        }),
    );
    let workspace_symbols = wait_for_response(&mut stdout, 3, Duration::from_secs(10));
    assert_eq!(
        workspace_symbols["result"],
        Value::Null,
        "workspace symbols must not expose project facts from text older than the open buffer: {workspace_symbols}"
    );

    let _ = server.0.kill();
}

#[test]
fn completion_drops_project_roots_when_checked_program_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("app.mw");
    let clean = "\
module app

resource Book
    required title: string

store ^books(id: int): Book

pub fn call(): int
    return 1
";
    std::fs::write(&file, clean).unwrap();

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
                    "text": clean
                }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &uri, Duration::from_secs(10));

    let edited = "module app\n\npub fn call(): int\n    return ^\n";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [ { "text": edited } ]
            }
        }),
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 3, "character": "    return ^".len() }
            }
        }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    let labels: Vec<&str> = response["result"]
        .as_array()
        .expect("completion result")
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect();
    assert!(
        !labels.contains(&"books"),
        "completion must not list saved roots from stale checked facts: {response}"
    );

    let _ = server.0.kill();
}

#[test]
fn semantic_tokens_drop_binding_facts_when_open_sibling_is_newer_than_cached_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let defs_file = src.join("defs.mw");
    let app_file = src.join("app.mw");
    let defs = "module defs\n\npub fn answer(): int\n    return 1\n";
    let app = "\
module app
use defs

pub fn call(): int
    return defs::answer()
";
    std::fs::write(&defs_file, defs).unwrap();
    std::fs::write(&app_file, app).unwrap();

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
    let initialize = recv(&mut stdout);
    let function_type = semantic_token_type_index(&initialize, "function");
    let variable_type = semantic_token_type_index(&initialize, "variable");
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

    let defs_uri = url::Url::from_file_path(&defs_file).unwrap().to_string();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": defs_uri,
                    "languageId": "marrow",
                    "version": 1,
                    "text": defs
                }
            }
        }),
    );
    let app_uri = url::Url::from_file_path(&app_file).unwrap().to_string();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": app_uri,
                    "languageId": "marrow",
                    "version": 1,
                    "text": app
                }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &app_uri, Duration::from_secs(10));

    let renamed = "module defs\n\npub fn renamed(): int\n    return 1\n";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": defs_uri, "version": 2 },
                "contentChanges": [ { "text": renamed } ]
            }
        }),
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/semanticTokens/full",
            "params": {
                "textDocument": { "uri": app_uri }
            }
        }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    let answer_type = semantic_token_type_at(&response, 4, "    return defs::".len() as u64)
        .expect("semantic token for answer call leaf");

    assert_eq!(
        answer_type, variable_type,
        "stale sibling snapshots must not color the call leaf as a binding-backed function: {response}"
    );
    assert_ne!(
        answer_type, function_type,
        "stale project BindingIndex facts must not be applied to semantic tokens: {response}"
    );

    let _ = server.0.kill();
}

#[test]
fn semantic_tokens_drop_identity_type_facts_when_closed_sibling_changes_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let schema_file = src.join("schema.mw");
    let app_file = src.join("app.mw");
    let schema = "\
module schema

resource Book
    title: string

store ^books(id: int): Book
";
    let app = "\
module app

fn f(id: Id(^books))
    return
";
    std::fs::write(&schema_file, schema).unwrap();
    std::fs::write(&app_file, app).unwrap();

    let (mut server, mut stdin, mut stdout, initialize) = initialized_server();
    let keyword_type = semantic_token_type_index(&initialize, "keyword");
    let struct_type = semantic_token_type_index(&initialize, "struct");

    let app_uri = url::Url::from_file_path(&app_file).unwrap().to_string();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": app_uri,
                    "languageId": "marrow",
                    "version": 1,
                    "text": app
                }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &app_uri, Duration::from_secs(10));

    std::fs::write(
        &schema_file,
        "\
module schema

resource Book
    title: string
",
    )
    .unwrap();

    let schema_uri = url::Url::from_file_path(&schema_file).unwrap().to_string();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "workspace/didChangeWatchedFiles",
            "params": {
                "changes": [{ "uri": schema_uri, "type": 2 }]
            }
        }),
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/semanticTokens/full",
            "params": {
                "textDocument": { "uri": app_uri }
            }
        }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    let id_type = semantic_token_type_at(&response, 2, "fn f(id: ".len() as u64)
        .expect("semantic token for Id constructor");

    assert_eq!(
        id_type, keyword_type,
        "closed-file disk edits must make checked identity type facts unavailable: {response}"
    );
    assert_ne!(
        id_type, struct_type,
        "stale identity type facts must not color Id as a struct: {response}"
    );

    let _ = server.0.kill();
}

#[test]
fn semantic_tokens_keep_identity_type_facts_after_unrelated_watched_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let project = root.join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let schema_file = project.join("src/schema.mw");
    let app_file = project.join("src/app.mw");
    let schema = "\
module schema

resource Book
    title: string

store ^books(id: int): Book
";
    let app = "\
module app

fn f(id: Id(^books))
    return
";
    std::fs::write(&schema_file, schema).unwrap();
    std::fs::write(&app_file, app).unwrap();

    let other_project = root.join("other");
    std::fs::create_dir_all(other_project.join("src")).unwrap();
    std::fs::write(
        other_project.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let other_file = other_project.join("src/other.mw");
    std::fs::write(
        &other_file,
        "module other\n\npub fn f(): int\n    return 1\n",
    )
    .unwrap();

    let (mut server, mut stdin, mut stdout, initialize) = initialized_server();
    let keyword_type = semantic_token_type_index(&initialize, "keyword");
    let struct_type = semantic_token_type_index(&initialize, "struct");

    let app_uri = url::Url::from_file_path(&app_file).unwrap().to_string();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": app_uri,
                    "languageId": "marrow",
                    "version": 1,
                    "text": app
                }
            }
        }),
    );
    let _ = wait_for_diagnostic_or_empty(&mut stdout, &app_uri, Duration::from_secs(10));

    std::fs::write(
        &other_file,
        "module other\n\npub fn changed(): int\n    return 1\n",
    )
    .unwrap();
    let other_uri = url::Url::from_file_path(&other_file).unwrap().to_string();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "workspace/didChangeWatchedFiles",
            "params": {
                "changes": [{ "uri": other_uri, "type": 2 }]
            }
        }),
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/semanticTokens/full",
            "params": {
                "textDocument": { "uri": app_uri }
            }
        }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    let id_type = semantic_token_type_at(&response, 2, "fn f(id: ".len() as u64)
        .expect("semantic token for Id constructor");

    assert_eq!(
        id_type, struct_type,
        "unrelated watched files must not clear current project identity type facts: {response}"
    );
    assert_ne!(
        id_type, keyword_type,
        "unrelated watched files should not make fresh checked facts unavailable: {response}"
    );

    let _ = server.0.kill();
}

#[test]
fn completion_after_caret_drops_saved_roots_without_fresh_checked_facts() {
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

    // Open a clean buffer declaring the saved resource and store, so the
    // analysis caches a program carrying that resource.
    let file = fixture_root().join("src/shelf/sample.mw");
    let uri = url::Url::from_file_path(&file).unwrap().to_string();
    let clean = "module shelf::sample\n\nresource Book\n    required title: string\n\nstore ^books(id: int): Book\n\npub fn drop()\n    return\n";
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
    // drops its module from a fresh analysis. Completion stays lexical, but must
    // not list saved roots from the previous checked program.
    let erroring = "module shelf::sample\n\nresource Book\n    required title: string\n\nstore ^books(id: int): Book\n\npub fn drop()\n    delete ^\n";
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

    // The `^` is the last non-newline character: line 8 (zero-based), after `delete `.
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/completion",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 8, "character": 12 }
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
        !labels.contains(&"books".to_string()),
        "completion after `^` must not list stale saved roots, got {labels:?}"
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

    // The custom saved-root request answers with a well-formed envelope. The
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
fn custom_saved_resource_inspector_reads_live_store_when_enabled() {
    let (_dir, file) = native_counter_fixture();
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
            "params": {
                "capabilities": {},
                "initializationOptions": { "marrow.liveData": true }
            }
        }),
    );
    let _ = recv(&mut stdout);
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

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

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "marrow/savedRoots"
        }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert!(response.get("error").is_none(), "no error: {response:?}");
    assert_eq!(response["result"]["available"], true);
    assert_eq!(
        response["result"]["roots"],
        json!([
            {
                "segment": { "kind": "root", "value": "counter" },
                "label": "counter"
            }
        ])
    );
    assert_store_snapshot(&response["result"]["store_snapshot"]);

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "marrow/dataChildren",
            "params": {
                "segments": [{ "kind": "root", "value": "counter" }],
                "limit": 10,
                "cursor": null
            }
        }),
    );
    let response = wait_for_response(&mut stdout, 3, Duration::from_secs(10));
    assert!(response.get("error").is_none(), "no error: {response:?}");
    assert_eq!(response["result"]["available"], true);
    assert_eq!(
        response["result"]["children"],
        json!([
            {
                "segment": { "kind": "key", "value": { "kind": "int", "value": 1 } },
                "label": "(1)"
            }
        ])
    );
    assert_store_snapshot(&response["result"]["store_snapshot"]);

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "marrow/dataRead",
            "params": {
                "segments": [
                    { "kind": "root", "value": "counter" },
                    { "kind": "key", "value": { "kind": "int", "value": 1 } },
                    { "kind": "field", "value": "value" }
                ]
            }
        }),
    );
    let response = wait_for_response(&mut stdout, 4, Duration::from_secs(10));
    assert!(response.get("error").is_none(), "no error: {response:?}");
    assert_eq!(response["result"]["available"], true);
    assert_eq!(response["result"]["presence"], "value_only");
    assert_eq!(response["result"]["value"], "42");
    assert_eq!(response["result"]["value_truncated"], false);
    assert_store_snapshot(&response["result"]["store_snapshot"]);

    let _ = server.0.kill();
}

#[test]
fn custom_data_watch_targets_return_project_paths_over_transport() {
    let (_dir, file) = native_counter_fixture();
    let root = file
        .parent()
        .and_then(|path| path.parent())
        .expect("fixture file lives under src");
    let expected_paths = json!([
        root.join("data").join("marrow.redb").to_string_lossy(),
        root.join("marrow.lock").to_string_lossy(),
    ]);

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
            "params": {
                "capabilities": {},
                "initializationOptions": { "marrow.liveData": true }
            }
        }),
    );
    let _ = recv(&mut stdout);
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

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

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "marrow/dataWatchTargets" }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert!(response.get("error").is_none(), "no error: {response:?}");
    assert_eq!(response["result"]["paths"], expected_paths, "{response}");

    let _ = server.0.kill();

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

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "marrow/dataWatchTargets" }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert!(response.get("error").is_none(), "no error: {response:?}");
    assert_eq!(response["result"]["paths"], json!([]), "{response}");

    let _ = server.0.kill();
}

#[test]
fn custom_saved_resource_inspector_refuses_stale_open_source() {
    let (_dir, file) = native_counter_fixture();
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
            "params": {
                "capabilities": {},
                "initializationOptions": { "marrow.liveData": true }
            }
        }),
    );
    let _ = recv(&mut stdout);
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

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

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "marrow/savedRoots" }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert_eq!(response["result"]["available"], true, "{response}");

    let edited = "module counter\n\npub fn f()\n    return\n";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": edited }]
            }
        }),
    );
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 3, "method": "marrow/savedRoots" }),
    );
    let response = wait_for_response(&mut stdout, 3, Duration::from_secs(10));
    assert_eq!(response["result"]["available"], false, "{response}");
    assert_eq!(
        response["result"]["roots"].as_array().map(Vec::len),
        Some(0),
        "{response}"
    );

    let _ = server.0.kill();
}

#[test]
fn custom_live_data_requests_refuse_recomputed_unsaved_source_identity_drift() {
    let (_dir, file) = native_counter_fixture();
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
            "params": {
                "capabilities": {},
                "initializationOptions": { "marrow.liveData": true }
            }
        }),
    );
    let _ = recv(&mut stdout);
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );

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

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "marrow/savedRoots" }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert_eq!(response["result"]["available"], true, "{response}");

    let edited = "module counter\n\npub fn f()\n    return\n";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": edited }]
            }
        }),
    );
    let diagnostics = wait_for_diagnostic_or_empty(&mut stdout, &uri, Duration::from_secs(10));
    assert_eq!(
        diagnostics["params"]["diagnostics"],
        json!([]),
        "the unsaved edit should recompute cleanly before live data is requested"
    );

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 3, "method": "marrow/savedRoots" }),
    );
    let response = wait_for_response(&mut stdout, 3, Duration::from_secs(10));
    assert!(response.get("error").is_none(), "no error: {response:?}");
    assert_eq!(response["result"]["available"], false, "{response}");
    assert_eq!(
        response["result"]["roots"].as_array().map(Vec::len),
        Some(0),
        "{response}"
    );
    assert_eq!(
        response["result"]["store_snapshot"],
        serde_json::Value::Null
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "marrow/dataChildren",
            "params": {
                "segments": [{ "kind": "root", "value": "counter" }],
                "limit": 10,
                "cursor": null
            }
        }),
    );
    let response = wait_for_response(&mut stdout, 4, Duration::from_secs(10));
    assert!(response.get("error").is_none(), "no error: {response:?}");
    assert_eq!(response["result"]["available"], false, "{response}");
    assert_eq!(
        response["result"]["children"].as_array().map(Vec::len),
        Some(0),
        "{response}"
    );
    assert_eq!(
        response["result"]["store_snapshot"],
        serde_json::Value::Null
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "marrow/dataRead",
            "params": {
                "segments": [
                    { "kind": "root", "value": "counter" },
                    { "kind": "key", "value": { "kind": "int", "value": 1 } },
                    { "kind": "field", "value": "value" }
                ]
            }
        }),
    );
    let response = wait_for_response(&mut stdout, 5, Duration::from_secs(10));
    assert!(response.get("error").is_none(), "no error: {response:?}");
    assert_eq!(response["result"]["available"], false, "{response}");
    assert_eq!(response["result"]["presence"], "absent", "{response}");
    assert_eq!(response["result"]["value"], serde_json::Value::Null);
    assert_eq!(
        response["result"]["store_snapshot"],
        serde_json::Value::Null
    );

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 6, "method": "marrow/dataIntegrity" }),
    );
    let response = wait_for_response(&mut stdout, 6, Duration::from_secs(10));
    assert!(response.get("error").is_none(), "no error: {response:?}");
    assert_eq!(response["result"]["available"], false, "{response}");
    assert_eq!(response["result"]["scanned"], 0, "{response}");
    assert_eq!(
        response["result"]["findings"].as_array().map(Vec::len),
        Some(0),
        "{response}"
    );
    assert_eq!(
        response["result"]["store_snapshot"],
        serde_json::Value::Null
    );

    let _ = server.0.kill();
}

#[test]
fn custom_data_requests_reject_invalid_typed_envelopes() {
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

    for (id, method, params) in [
        (
            2,
            "marrow/dataChildren",
            json!({ "segments": [], "limit": 1, "cursor": null }),
        ),
        (
            3,
            "marrow/dataChildren",
            json!({ "segments": [{ "kind": "root", "value": "counter" }], "limit": 0 }),
        ),
        (4, "marrow/dataRead", json!({ "segments": [] })),
        (
            5,
            "marrow/dataRead",
            json!({
                "segments": [{ "kind": "root", "value": "counter" }],
                "preview_limit": 0
            }),
        ),
    ] {
        send(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }),
        );
        let response = wait_for_response(&mut stdout, id, Duration::from_secs(10));
        assert_eq!(
            response["error"]["code"], -32602,
            "invalid {method} envelope should fail before project loading, got {response}"
        );
        assert!(
            response.get("result").is_none(),
            "invalid {method} envelope must not include a result: {response}"
        );
    }

    let _ = server.0.kill();
}

#[test]
fn custom_data_integrity_is_unavailable_without_live_data_opt_in() {
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
    let source = "module shelf\n\nresource Book\n    required title: string\n\nstore ^books(id: int): Book\n\npub fn f()\n    return\n";
    let file = src.join("shelf.mw");
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

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "marrow/dataIntegrity" }),
    );
    let response = wait_for_response(&mut stdout, 2, Duration::from_secs(10));
    assert!(response.get("error").is_none(), "no error: {response:?}");
    let result = &response["result"];
    assert_eq!(
        result["available"].as_bool(),
        Some(false),
        "missing marrow.liveData opt-in should not read the store"
    );
    assert_eq!(
        result["scanned"].as_u64(),
        Some(0),
        "no store entries should be scanned"
    );
    assert_eq!(result["findings"].as_array().map(Vec::len), Some(0));
    assert_eq!(result["truncated"].as_bool(), Some(false));
    assert_eq!(result["store_snapshot"], serde_json::Value::Null);

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

fn semantic_token_type_index(initialize: &Value, name: &str) -> u64 {
    initialize["result"]["capabilities"]["semanticTokensProvider"]["legend"]["tokenTypes"]
        .as_array()
        .expect("semantic token legend")
        .iter()
        .position(|token_type| token_type.as_str() == Some(name))
        .unwrap_or_else(|| panic!("semantic token type {name:?} should be advertised")) as u64
}

fn semantic_token_type_at(response: &Value, target_line: u64, target_start: u64) -> Option<u64> {
    let data = response["result"]["data"].as_array()?;
    let mut line = 0u64;
    let mut start = 0u64;
    for chunk in data.chunks_exact(5) {
        let delta_line = chunk[0].as_u64()?;
        let delta_start = chunk[1].as_u64()?;
        let token_type = chunk[3].as_u64()?;
        if delta_line == 0 {
            start += delta_start;
        } else {
            line += delta_line;
            start = delta_start;
        }
        if line == target_line && start == target_start {
            return Some(token_type);
        }
    }
    None
}
