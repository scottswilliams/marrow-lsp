//! End-to-end smoke test: launch the built MCP server as a subprocess, speak the
//! real MCP stdio framing (newline-delimited JSON-RPC), initialize it, list its
//! tools, and call `mw_check` against a real project — proving the transport, the
//! tool routing, and the core analysis work together, not just that it builds.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Write one JSON-RPC message as a single line — the MCP stdio framing.
fn send(stdin: &mut impl Write, message: &Value) {
    let line = serde_json::to_string(message).unwrap();
    writeln!(stdin, "{line}").unwrap();
    stdin.flush().unwrap();
}

/// Read messages until the response with `id` arrives, returning it.
fn wait_for(reader: &mut impl BufRead, id: i64, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(line.trim()).expect("a JSON message per line");
        if message["id"] == id {
            return message;
        }
    }
    panic!("no response for request {id} within the timeout");
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A throwaway project on disk: a `marrow.json` and a clean module, returning its
/// module file path. The temp dir is leaked so it outlives the child process; the
/// OS reclaims it on exit.
fn temp_project() -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
    let src = root.join("src/shelf");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("books.mw");
    std::fs::write(
        &file,
        "module shelf::books\n\npub fn double(n: int): int\n    return n * 2\n",
    )
    .unwrap();
    std::mem::forget(dir);
    file
}

#[test]
fn initialize_list_tools_then_call_mw_check() {
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_marrow-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the marrow-mcp binary runs"),
    );
    let mut stdin = server.0.stdin.take().unwrap();
    let mut stdout = BufReader::new(server.0.stdout.take().unwrap());

    // initialize → the server names itself and advertises tools.
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {} } }),
    );
    let response = wait_for(&mut stdout, 1, Duration::from_secs(10));
    assert_eq!(response["result"]["serverInfo"]["name"], "marrow-mcp");
    assert!(
        response["result"]["capabilities"]["tools"].is_object(),
        "the server advertises tools, got {response}"
    );

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    );

    // tools/list → the catalog includes mw_check and mw_run.
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    );
    let response = wait_for(&mut stdout, 2, Duration::from_secs(10));
    let tools = response["result"]["tools"]
        .as_array()
        .expect("a tools array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(
        names.contains(&"mw_check"),
        "mw_check listed, got {names:?}"
    );
    assert!(names.contains(&"mw_run"), "mw_run listed, got {names:?}");
    assert!(
        names.contains(&"mw_data_integrity"),
        "mw_data_integrity listed, got {names:?}"
    );

    // tools/call mw_check against a project file overlaid with a type error.
    let file = temp_project();
    let erroring = "module shelf::books\n\npub fn double(n: int): int\n    return \"nope\"\n";
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "mw_check",
                "arguments": { "file": file.to_string_lossy(), "source": erroring }
            }
        }),
    );
    let response = wait_for(&mut stdout, 3, Duration::from_secs(10));
    assert!(
        response.get("error").is_none(),
        "a tool call must not be a protocol error: {response}"
    );
    let result = &response["result"];
    assert_eq!(result["isError"], false);
    let diagnostics = result["structuredContent"]["diagnostics"]
        .as_array()
        .expect("a diagnostics array");
    assert!(
        diagnostics.iter().any(|d| d["code"] == "check.return_type"),
        "the overlay's return-type error should surface, got {result}"
    );

    let _ = server.0.kill();
}

#[test]
fn data_tools_refuse_without_the_opt_in() {
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_marrow-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // No --allow-data and no env var: the data tools must refuse.
            .env_remove("MARROW_MCP_ALLOW_DATA")
            .spawn()
            .expect("the marrow-mcp binary runs"),
    );
    let mut stdin = server.0.stdin.take().unwrap();
    let mut stdout = BufReader::new(server.0.stdout.take().unwrap());

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
    );
    let _ = wait_for(&mut stdout, 1, Duration::from_secs(10));

    let file = temp_project();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "mw_saved_roots", "arguments": { "file": file.to_string_lossy() } }
        }),
    );
    let response = wait_for(&mut stdout, 2, Duration::from_secs(10));
    let structured = &response["result"]["structuredContent"];
    assert_eq!(
        structured["dataAccess"], "disabled",
        "without the opt-in, data tools must refuse and read nothing, got {response}"
    );
    assert_eq!(structured["available"], false);
    assert_eq!(structured["contract"]["status"], "blocked-on-marrow");
    assert_eq!(
        structured["contract"]["description"],
        "debug/admin prototype"
    );
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("debug/admin prototype (blocked-on-marrow: "),
        "summary must name the prototype contract, got {response}"
    );

    let _ = server.0.kill();
}

#[test]
fn mw_data_integrity_flags_an_orphan_with_the_opt_in() {
    use marrow_store::backend::Backend;
    use marrow_store::path::{PathSegment, SavedKey, encode_path};
    use marrow_store::redb::RedbStore;

    // A throwaway project pinning a native store. The schema declares
    // `^books(id).title`; the store also holds `^books(1).sticker`, an undeclared
    // field, so the schema-impact advisory must flag it as an orphan.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" } }"#,
    )
    .unwrap();
    let src = root.join("src/shelf");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    let file = src.join("books.mw");
    std::fs::write(
        &file,
        "module shelf::books\n\nresource Book at ^books(id: int)\n    required title: string\n",
    )
    .unwrap();
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
    // Keep the project dir alive for the child process's lifetime; the OS reclaims
    // it on exit.
    std::mem::forget(dir);

    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_marrow-mcp"))
            .arg("--allow-data")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the marrow-mcp binary runs"),
    );
    let mut stdin = server.0.stdin.take().unwrap();
    let mut stdout = BufReader::new(server.0.stdout.take().unwrap());

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
    );
    let _ = wait_for(&mut stdout, 1, Duration::from_secs(10));

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "mw_data_integrity", "arguments": { "file": file.to_string_lossy() } }
        }),
    );
    let response = wait_for(&mut stdout, 2, Duration::from_secs(10));
    let structured = &response["result"]["structuredContent"];
    assert_eq!(
        structured["available"], true,
        "the seeded store is readable with the opt-in, got {response}"
    );
    let findings = structured["findings"].as_array().expect("a findings array");
    assert_eq!(findings.len(), 1, "one orphan finding: {response}");
    assert_eq!(findings[0]["kind"], "orphan");
    assert_eq!(findings[0]["path"], "^books(1).sticker");
    assert_eq!(structured["contract"]["status"], "presentation-only");
    assert_eq!(
        structured["contract"]["description"],
        "debug/admin advisory"
    );
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("debug/admin advisory (presentation-only: "),
        "summary must name the advisory contract, got {response}"
    );

    let _ = server.0.kill();
}

#[test]
fn mw_run_executes_over_a_sandboxed_store() {
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_marrow-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the marrow-mcp binary runs"),
    );
    let mut stdin = server.0.stdin.take().unwrap();
    let mut stdout = BufReader::new(server.0.stdout.take().unwrap());

    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
    );
    let _ = wait_for(&mut stdout, 1, Duration::from_secs(10));

    let file = temp_project();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "mw_run",
                "arguments": { "file": file.to_string_lossy(), "entry": "shelf::books::double", "args": [21], "allowPrototypeArgs": true }
            }
        }),
    );
    let response = wait_for(&mut stdout, 2, Duration::from_secs(10));
    assert_eq!(
        response["result"]["structuredContent"]["value"], 42,
        "double(21) over a fresh MemStore returns 42, got {response}"
    );
    assert_eq!(
        response["result"]["structuredContent"]["contract"]["status"],
        "presentation-only"
    );
    assert_eq!(
        response["result"]["structuredContent"]["contract"]["description"],
        "debug/admin prototype"
    );
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("debug/admin prototype (presentation-only: "),
        "summary must name the run prototype contract, got {response}"
    );

    let _ = server.0.kill();
}
