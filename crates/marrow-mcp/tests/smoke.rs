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
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src/shelf");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("books.mw");
    std::fs::write(
        &file,
        "module shelf::books\n\npub fn double(n: int): int\n    return n * 2\n\npub fn answer(): int\n    return 42\n",
    )
    .unwrap();
    std::mem::forget(dir);
    file
}

fn temp_project_with_tests() -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" }, "tests": ["tests"] }"#,
    )
    .unwrap();
    let src = root.join("src/shelf");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("books.mw");
    std::fs::write(
        &file,
        "module shelf::books\n\npub fn answer(): int\n    return 42\n",
    )
    .unwrap();
    let tests = root.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(
        tests.join("smoke_test.mw"),
        "use shelf::books\n\npub fn smoke()\n    std::assert::isTrue(books::answer() == 42)\n",
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
        !names.contains(&"mw_data_integrity"),
        "retired advisory data-integrity tool must not be listed, got {names:?}"
    );
    assert!(
        names.contains(&"mw_data_children"),
        "mw_data_children listed, got {names:?}"
    );
    assert!(
        names.contains(&"mw_data_read"),
        "mw_data_read listed, got {names:?}"
    );
    assert!(
        names.contains(&"mw_surface_routes"),
        "mw_surface_routes listed, got {names:?}"
    );
    assert!(
        names.contains(&"mw_surface_read"),
        "mw_surface_read listed, got {names:?}"
    );
    assert!(
        names.contains(&"mw_surface_write"),
        "mw_surface_write listed, got {names:?}"
    );
    assert!(
        !names.contains(&"mw_saved_get") && !names.contains(&"mw_saved_children"),
        "old saved-data child/get tools stay absent, got {names:?}"
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
    assert_eq!(structured["contract"]["status"], "ready");
    assert_eq!(structured["contract"]["stableProductionApi"], true);
    assert_eq!(
        structured["contract"]["description"],
        "saved-root listing API"
    );
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("saved-root listing API (ready): "),
        "summary must name the saved-root contract, got {response}"
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "mw_data_children",
                "arguments": {
                    "file": "/nope/project/src/main.mw",
                    "segments": [
                        { "kind": "root", "store_catalog_id": "cat_00000000000000000000000000000001" }
                    ],
                    "limit": 1
                }
            }
        }),
    );
    let response = wait_for(&mut stdout, 3, Duration::from_secs(10));
    let structured = &response["result"]["structuredContent"];
    assert_eq!(
        structured["dataAccess"], "disabled",
        "without the opt-in, mw_data_children must refuse before loading the file, got {response}"
    );
    assert_eq!(structured["available"], false);
    assert_eq!(structured["contract"]["status"], "ready");
    assert_eq!(structured["contract"]["stableProductionApi"], true);
    assert_eq!(
        structured["contract"]["description"],
        "bounded typed data children API"
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "mw_data_read",
                "arguments": {
                    "file": "/nope/project/src/main.mw",
                    "segments": [
                        { "kind": "root", "store_catalog_id": "cat_00000000000000000000000000000001" }
                    ]
                }
            }
        }),
    );
    let response = wait_for(&mut stdout, 4, Duration::from_secs(10));
    let structured = &response["result"]["structuredContent"];
    assert_eq!(
        structured["dataAccess"], "disabled",
        "without the opt-in, mw_data_read must refuse before loading the file, got {response}"
    );
    assert_eq!(structured["available"], false);
    assert_eq!(structured["contract"]["status"], "ready");
    assert_eq!(structured["contract"]["stableProductionApi"], true);
    assert_eq!(
        structured["contract"]["description"],
        "bounded typed data read API"
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "mw_surface_read",
                "arguments": {
                    "file": "/nope/project/src/main.mw",
                    "operation": {
                        "profile_version": "surface.operation.v1",
                        "operation_tag": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                        "request": { "kind": "singleton_read" }
                    }
                }
            }
        }),
    );
    let response = wait_for(&mut stdout, 5, Duration::from_secs(10));
    let structured = &response["result"]["structuredContent"];
    assert_eq!(
        structured["dataAccess"], "disabled",
        "without the opt-in, mw_surface_read must refuse before loading the file, got {response}"
    );
    assert_eq!(structured["available"], false);
    assert_eq!(structured["contract"]["status"], "ready");
    assert_eq!(
        structured["contract"]["description"],
        "surface read operation"
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "mw_surface_write",
                "arguments": {
                    "file": "/nope/project/src/main.mw",
                    "operation": {
                        "profile_version": "surface.operation.v1",
                        "operation_tag": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                        "request": {
                            "kind": "point_update",
                            "request": {
                                "identity": {
                                    "store_catalog_id": "cat_00000000000000000000000000000000",
                                    "keys": [{ "kind": "int", "value": "1" }]
                                },
                                "fields": []
                            }
                        }
                    }
                }
            }
        }),
    );
    let response = wait_for(&mut stdout, 6, Duration::from_secs(10));
    let structured = &response["result"]["structuredContent"];
    assert_eq!(
        structured["dataAccess"], "disabled",
        "without the opt-in, mw_surface_write must refuse before loading the file, got {response}"
    );
    assert_eq!(structured["available"], false);
    assert_eq!(structured["contract"]["status"], "ready");
    assert_eq!(
        structured["contract"]["description"],
        "surface write operation"
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
                "arguments": { "file": file.to_string_lossy(), "entry": "shelf::books::answer" }
            }
        }),
    );
    let response = wait_for(&mut stdout, 2, Duration::from_secs(10));
    assert_eq!(
        response["result"]["structuredContent"]["result"]["value"],
        json!({ "kind": "int", "value": 42 }),
        "answer() over a fresh store returns 42, got {response}"
    );
    assert_eq!(
        response["result"]["structuredContent"]["contract"]["status"],
        "ready"
    );
    assert_eq!(
        response["result"]["structuredContent"]["contract"]["description"],
        "sandboxed execution API"
    );
    assert_eq!(
        response["result"]["structuredContent"]["runFacts"]["executionBoundary"]["store"]["kind"],
        "fresh_memory",
        "mw_run must surface Marrow's execution boundary, got {response}"
    );
    assert_eq!(
        response["result"]["structuredContent"]["runFacts"].get("analysis"),
        None,
        "mw_run must use executionBoundary as the only analysis generation carrier, got {response}"
    );
    assert_eq!(
        response["result"]["structuredContent"]["runFacts"]["executionBoundary"]["sourceAnalysisGeneration"]
            ["profileVersion"],
        "analysis.generation.v1",
        "mw_run must surface Marrow's boundary generation, got {response}"
    );
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("sandboxed execution API (ready): "),
        "summary must name the run contract, got {response}"
    );

    let file = temp_project_with_tests();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "mw_run",
                "arguments": { "file": file.to_string_lossy(), "mode": "test" }
            }
        }),
    );
    let response = wait_for(&mut stdout, 3, Duration::from_secs(10));
    assert_eq!(
        response["result"]["structuredContent"]["executionBoundary"]["sessionKind"], "test",
        "mw_run test mode must surface the Marrow test boundary, got {response}"
    );
    assert_eq!(
        response["result"]["structuredContent"]["executionBoundary"]["sourceAnalysisGeneration"]["profileVersion"],
        "analysis.generation.v1",
        "mw_run test mode must carry Marrow's source analysis generation, got {response}"
    );
    assert_eq!(
        response["result"]["structuredContent"]["executionBoundary"]["store"]["kind"],
        "test_memory",
        "mw_run test mode must report Marrow's test-memory boundary, got {response}"
    );
    assert_eq!(
        response["result"]["structuredContent"]["executionBoundary"]["store"]["stamp"],
        json!(null),
        "test-memory boundary must not fabricate a store stamp, got {response}"
    );

    let _ = server.0.kill();
}
