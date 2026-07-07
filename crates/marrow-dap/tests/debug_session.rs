//! End-to-end debug session over the real `marrow-dap` binary.
//!
//! Spawns the built binary, drives the DAP handshake over its stdio with
//! `Content-Length` framing, launches a fixture entry with stop-on-entry, and
//! asserts the run stops, exposes live locals and durable data, then continues to
//! a clean termination. This exercises the whole rendezvous through the wire
//! protocol, not internal APIs.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use marrow_project::CATALOG_FILE_NAME;
use marrow_run::{Host, ProjectOpen, ProjectSession, SessionEntry};
use marrow_store::{AccessMode, SealedStore};
use serde_json::{Value as Json, json};

const SHELF_SOURCE: &str = "module shelf\n\
                            \n\
                            pub fn main()\n\
                            \x20   const id: int = 1\n\
                            \x20   const title: string = \"Dune\"\n\
                            \x20   print(title)\n";

/// A minimal DAP client over a child process's stdio.
struct Client {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
    seq: i64,
}

impl Client {
    /// Launch the built `marrow-dap` binary and wrap its stdio.
    fn spawn() -> Self {
        Self::spawn_with_cwd(None)
    }

    fn spawn_in(cwd: &Path) -> Self {
        Self::spawn_with_cwd(Some(cwd))
    }

    fn spawn_with_cwd(cwd: Option<&Path>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_marrow-dap"));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().expect("spawn marrow-dap");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Client {
            child,
            stdin: Some(stdin),
            stdout,
            seq: 0,
        }
    }

    /// The child adapter process id, used to locate its isolated-store scratch.
    fn child_id(&self) -> u32 {
        self.child.id()
    }

    /// Close the adapter's input stream, signalling EOF mid-session, then wait for
    /// the process to exit so teardown has completed before assertions run.
    fn close_input_and_wait(&mut self) {
        self.stdin = None;
        self.child.wait().expect("adapter exits after input EOF");
    }

    /// Send a DAP request and return its sequence number.
    fn request(&mut self, command: &str, arguments: Json) -> i64 {
        self.request_with_arguments(command, Some(arguments))
    }

    fn request_without_arguments(&mut self, command: &str) -> i64 {
        self.request_with_arguments(command, None)
    }

    fn request_with_arguments(&mut self, command: &str, arguments: Option<Json>) -> i64 {
        self.seq += 1;
        let seq = self.seq;
        let mut message = json!({
            "seq": seq,
            "type": "request",
            "command": command,
        });
        if let Some(arguments) = arguments {
            message["arguments"] = arguments;
        }
        let body = serde_json::to_vec(&message).unwrap();
        let stdin = self.stdin.as_mut().expect("adapter input is open");
        write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        stdin.write_all(&body).unwrap();
        stdin.flush().unwrap();
        seq
    }

    /// Read one framed message from the server.
    fn read(&mut self) -> Json {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("read header");
            assert_ne!(read, 0, "server closed the stream unexpectedly");
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                content_length = value.trim().parse().ok();
            }
        }
        let length = content_length.expect("a Content-Length header");
        let mut body = vec![0u8; length];
        self.stdout.read_exact(&mut body).expect("read body");
        serde_json::from_slice(&body).unwrap()
    }

    /// Read messages until one satisfies `predicate`, returning it. Useful for
    /// skipping interleaved events while waiting for a specific response or event.
    fn read_until(&mut self, predicate: impl Fn(&Json) -> bool) -> Json {
        for _ in 0..100 {
            let message = self.read();
            if predicate(&message) {
                return message;
            }
        }
        panic!("did not observe the awaited message within the bound");
    }

    /// Read the response to a specific request sequence.
    fn response_for(&mut self, request_seq: i64) -> Json {
        self.read_until(|message| {
            message["type"] == "response" && message["request_seq"] == request_seq
        })
    }

    /// Read the next event with the given name.
    fn event(&mut self, name: &str) -> Json {
        self.read_until(|message| message["type"] == "event" && message["event"] == name)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Write a self-contained fixture project with locals the debugger can inspect
/// at a stepped stop.
fn write_fixture(dir: &Path) -> std::path::PathBuf {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("shelf.mw");
    std::fs::write(&file, SHELF_SOURCE).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"memory\" } }",
    )
    .unwrap();
    file
}

fn write_durable_helper_fixture(dir: &Path) -> std::path::PathBuf {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let source = "module shelf\n\
                  \n\
                  resource Book\n\
                  \x20   required title: string\n\
                  store ^books(id: int): Book\n\
                  fn storedTitle(id: int): string\n\
                  \x20   return ^books(id).title ?? \"\"\n\
                  pub fn main()\n\
                  \x20   const id: int = 1\n\
                  \x20   const title: string = \"Dune\"\n\
                  \x20   print(title)\n";
    let file = src.join("shelf.mw");
    std::fs::write(&file, source).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"native\", \"dataDir\": \"data\" } }",
    )
    .unwrap();
    file
}

fn write_durable_debug_data_fixture(dir: &Path) -> std::path::PathBuf {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let source = "module shelf\n\
                  \n\
                  resource Book\n\
                  \x20   required title: string\n\
                  store ^books(id: int): Book\n\
                  pub fn main()\n\
                  \x20   const id: int = 1\n\
                  \x20   ^books(id).title = \"Dune\"\n\
                  \x20   print(^books(id).title ?? \"\")\n";
    let file = src.join("shelf.mw");
    std::fs::write(&file, source).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"native\", \"dataDir\": \"data\" } }",
    )
    .unwrap();
    file
}

fn write_durable_debug_transaction_fixture(dir: &Path) -> std::path::PathBuf {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let source = "module shelf\n\
                  \n\
                  resource Book\n\
                  \x20   required title: string\n\
                  store ^books(id: int): Book\n\
                  pub fn main()\n\
                  \x20   ^books(1).title = \"draft\"\n\
                  \x20   transaction\n\
                  \x20       ^books(1).title = \"Dune\"\n\
                  \x20       print(^books(1).title ?? \"\")\n";
    let file = src.join("shelf.mw");
    std::fs::write(&file, source).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"native\", \"dataDir\": \"data\" } }",
    )
    .unwrap();
    file
}

fn write_durable_debug_data_paging_fixture(dir: &Path) -> std::path::PathBuf {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let source = "module shelf\n\
                  \n\
                  resource Book\n\
                  \x20   required title: string\n\
                  store ^books(id: int): Book\n\
                  pub fn main()\n\
                  \x20   ^books(1).title = \"Dune\"\n\
                  \x20   ^books(2).title = \"Hyperion\"\n\
                  \x20   ^books(3).title = \"Solaris\"\n\
                  \x20   print(^books(3).title ?? \"\")\n";
    let file = src.join("shelf.mw");
    std::fs::write(&file, source).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"native\", \"dataDir\": \"data\" } }",
    )
    .unwrap();
    file
}

fn write_repeated_call_fixture(dir: &Path) -> std::path::PathBuf {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let source = "module shelf\n\
                  \n\
                  fn hit()\n\
                  \x20   print(\"hit\")\n\
                  \n\
                  pub fn main()\n\
                  \x20   hit()\n\
                  \x20   hit()\n\
                  \x20   print(\"done\")\n";
    let file = src.join("shelf.mw");
    std::fs::write(&file, source).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"memory\" } }",
    )
    .unwrap();
    file
}

#[cfg(unix)]
fn write_symlink_source_root_fixture(dir: &Path) -> std::path::PathBuf {
    let real_src = dir.join("real_src");
    std::fs::create_dir_all(&real_src).unwrap();
    let file = real_src.join("shelf.mw");
    std::fs::write(&file, SHELF_SOURCE).unwrap();
    std::os::unix::fs::symlink(&real_src, dir.join("src_link")).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src_link\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"memory\" } }",
    )
    .unwrap();
    file
}

fn initialized_client() -> Client {
    let mut client = Client::spawn();
    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);
    client
}

fn stopped_fixture_client() -> (tempfile::TempDir, Client) {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());
    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": true,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");

    (dir, client)
}

fn entry_stopped_client_with_breakpoint(
    line: u32,
) -> (tempfile::TempDir, std::path::PathBuf, Client) {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": true,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": line }],
        }),
    );
    assert_eq!(client.response_for(set)["success"], true);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");

    (dir, file, client)
}

/// Write a fixture whose entry accepts a typed string argument.
fn write_arg_fixture(dir: &Path) {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let source = "module shelf\n\
                  \n\
                  pub fn default()\n\
                  \x20   print(\"default\")\n\
                  \n\
                  pub fn main(title: string)\n\
                  \x20   print(title)\n";
    std::fs::write(src.join("shelf.mw"), source).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::default\" }, \"store\": { \"backend\": \"memory\" } }",
    )
    .unwrap();
}

fn write_native_identity_fixture(dir: &Path) {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let source = "module shelf\n\
                  \n\
                  resource Book\n\
                  \x20   required title: string\n\
                  \n\
                  store ^books(id: int): Book\n\
                  \n\
                  pub fn main()\n\
                  \x20   ^books(1).title = \"Dune\"\n\
                  \x20   print(\"seeded\")\n";
    std::fs::write(src.join("shelf.mw"), source).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"native\", \"dataDir\": \"data\" } }",
    )
    .unwrap();
}

fn seed_native_store(dir: &Path) {
    let session = ProjectSession::open(dir, ProjectOpen::run()).expect("seed session");
    let host = Host::new();
    let mut output = String::new();
    session
        .invoke(SessionEntry::new("shelf::main", &host, &mut output))
        .expect("seed run");

    let catalog = dir.join(marrow_project::CATALOG_FILE_NAME);
    assert!(catalog.exists(), "seed run should write committed lock");
    assert!(
        dir.join("data").join("marrow.redb").exists(),
        "seed run should create the native store"
    );
}

fn commit_proposed_catalog_lock(dir: &Path) {
    let config = marrow_check::load_config(dir).expect("project config");
    let (_report, program) = marrow_check::check_project(dir, &config).expect("check project");
    let proposal = program
        .catalog
        .proposal
        .as_ref()
        .expect("native resource analysis should propose catalog identity")
        .clone();
    let source_digest = program.source_digest();
    marrow_check::project_store_lock(dir, &proposal, &source_digest).expect("project lock");

    assert!(
        dir.join(CATALOG_FILE_NAME).exists(),
        "project lock should be committed"
    );
}

fn corrupt_catalog_utf8_with_empty_native_store(dir: &Path) {
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    drop(SealedStore::open(&data_dir.join("marrow.redb"), AccessMode::Create).unwrap());
    let catalog = dir.join(marrow_project::CATALOG_FILE_NAME);
    std::fs::write(catalog, [0xff]).expect("write invalid UTF-8 catalog");
}

/// Write a fixture with an expandable local sequence so DAP value responses
/// exercise the local expansion contract.
fn write_value_contract_fixture(dir: &Path) -> std::path::PathBuf {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let source = "module shelf\n\
                  \n\
                  pub fn main()\n\
                  \x20   const id: int = 1\n\
                  \x20   const title: string = \"Dune\"\n\
                  \x20   var tags: sequence[string]\n\
                  \x20   tags(1) = \"classic\"\n\
                  \x20   tags(2) = \"space\"\n\
                  \x20   tags(3) = \"award\"\n\
                  \x20   print(title)\n";
    let file = src.join("shelf.mw");
    std::fs::write(&file, source).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"memory\" } }",
    )
    .unwrap();
    file
}

fn write_large_sequence_fixture(dir: &Path, item_count: usize) -> (std::path::PathBuf, u32) {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut source = String::from("module shelf\n\npub fn main()\n   var items: sequence[int]\n");
    for index in 1..=item_count {
        source.push_str(&format!("   items({index}) = {index}\n"));
    }
    let print_line = item_count as u32 + 5;
    source.push_str("   print(\"done\")\n");
    let file = src.join("shelf.mw");
    std::fs::write(&file, source).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"memory\" } }",
    )
    .unwrap();
    (file, print_line)
}

fn assert_runtime_debug_value(value: &Json) {
    assert!(
        value.get("type").is_none(),
        "runtime debug metadata should not masquerade as a value type: {value}"
    );

    let attributes = value["presentationHint"]["attributes"]
        .as_array()
        .unwrap_or_else(|| panic!("runtime debug value should include attributes: {value}"));
    assert!(
        attributes.iter().any(|attribute| attribute == "readOnly"),
        "runtime debug value presentation should be read-only: {value}"
    );
    assert!(
        value.get("marrowContract").is_none(),
        "runtime debug values should consume Marrow facts directly: {value}"
    );
}

fn assert_runtime_store_snapshot(snapshot: &Json) {
    let object = snapshot
        .as_object()
        .unwrap_or_else(|| panic!("store snapshot should be an object: {snapshot}"));
    assert_eq!(
        snapshot["profile_version"], "data.generation.v1",
        "runtime data variables should carry Marrow data generation metadata: {snapshot}"
    );
    assert!(
        object.contains_key("store_uid"),
        "runtime data variables should carry the optional store UID field: {snapshot}"
    );
    assert!(
        object.contains_key("catalog_digest"),
        "runtime data variables should carry the optional catalog digest field: {snapshot}"
    );
    assert!(
        snapshot["checked_source_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "runtime data variables should carry the checked source digest: {snapshot}"
    );
    assert!(
        snapshot["commit"]["commit_id"].is_u64(),
        "stopped-frame data reads should identify the runtime store commit: {snapshot}"
    );
    assert!(
        snapshot["commit"]["source_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "runtime store commit should carry the committed source digest: {snapshot}"
    );
    assert!(object.contains_key("open_transaction"));
}

fn assert_runtime_open_transaction_snapshot(snapshot: &Json, depth: i64) {
    let object = snapshot
        .as_object()
        .unwrap_or_else(|| panic!("store snapshot should be an object: {snapshot}"));
    assert_eq!(
        snapshot["profile_version"], "data.generation.v1",
        "runtime data variables should carry Marrow data generation metadata: {snapshot}"
    );
    assert!(
        object.contains_key("store_uid"),
        "runtime data variables should carry the optional store UID field: {snapshot}"
    );
    assert!(
        object.contains_key("catalog_digest"),
        "runtime data variables should carry the optional catalog digest field: {snapshot}"
    );
    assert!(
        snapshot["checked_source_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "runtime data variables should carry the checked source digest: {snapshot}"
    );
    assert!(
        snapshot["commit"].is_null(),
        "open transaction data reads should not claim a committed store snapshot: {snapshot}"
    );
    assert_eq!(
        snapshot["open_transaction"]["depth"], depth,
        "open transaction data reads should identify the stopped transaction depth: {snapshot}"
    );
}

fn assert_debug_data_boundary(boundary: &Json, store_snapshot: &Json) {
    assert_eq!(
        boundary["sourceAnalysisGeneration"]["profileVersion"], "analysis.generation.v1",
        "debug data boundaries should carry Marrow source analysis generation: {boundary}"
    );
    assert!(
        boundary["sourceAnalysisGeneration"]["sourceIdentity"]
            .as_str()
            .is_some_and(|identity| identity.starts_with("sha256:")),
        "debug data boundaries should carry source identity: {boundary}"
    );
    assert_eq!(
        boundary["storeSnapshot"], *store_snapshot,
        "debug data boundaries should bind the returned store snapshot: {boundary}"
    );
}

fn assert_evaluate_runtime_debug_value(response: &Json, result: &str, variables_reference: i64) {
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["body"]["result"], result, "{response}");
    assert_eq!(
        response["body"]["variablesReference"], variables_reference,
        "{response}"
    );
    assert_runtime_debug_value(&response["body"]);
    assert!(
        response["body"].get("marrowError").is_none(),
        "successful evaluate responses should not carry an error contract: {response}"
    );
}

fn assert_response_marrow_error(
    response: &Json,
    code: &str,
    status: &str,
    blocked_on: Option<&str>,
) {
    assert_eq!(response["success"], false, "{response}");
    let has_display_message = response["message"]
        .as_str()
        .map(|text| !text.is_empty())
        .unwrap_or(false);
    assert!(
        has_display_message,
        "failed DAP responses should keep a client display message: {response}"
    );

    let contract = response["body"]["marrowError"]
        .as_object()
        .unwrap_or_else(|| panic!("failed DAP response should include marrowError: {response}"));
    assert_eq!(
        contract.get("code").and_then(Json::as_str),
        Some(code),
        "DAP error should expose stable code `{code}`: {response}"
    );
    assert_eq!(
        contract.get("status").and_then(Json::as_str),
        Some(status),
        "DAP error should expose stable status `{status}`: {response}"
    );
    match blocked_on {
        Some(missing_fact) => assert_eq!(
            contract.get("blockedOn").and_then(Json::as_str),
            Some(missing_fact),
            "DAP error should name `{missing_fact}`: {response}"
        ),
        None => assert!(
            contract.get("blockedOn").is_none(),
            "DAP error should not expose blockedOn when it is not blocked on Marrow: {response}"
        ),
    }
}

fn assert_verified_breakpoint(breakpoint: &Json, line: i64) {
    assert_eq!(breakpoint["verified"], true, "{breakpoint}");
    assert_eq!(breakpoint["line"], line, "{breakpoint}");
    assert!(
        breakpoint.get("marrowContract").is_none(),
        "verified breakpoints should not carry a blocked Marrow contract: {breakpoint}"
    );
}

fn assert_breakpoint_contract(
    breakpoint: &Json,
    code: &str,
    status: &str,
    blocked_on: Option<&str>,
) {
    let contract = breakpoint["marrowContract"].as_object().unwrap_or_else(|| {
        panic!("unverified breakpoint should include marrowContract: {breakpoint}")
    });
    assert_eq!(
        contract.get("code").and_then(Json::as_str),
        Some(code),
        "breakpoint should expose stable code `{code}`: {breakpoint}"
    );
    assert_eq!(
        contract.get("status").and_then(Json::as_str),
        Some(status),
        "breakpoint should expose stable status `{status}`: {breakpoint}"
    );
    match blocked_on {
        Some(missing_fact) => assert_eq!(
            contract.get("blockedOn").and_then(Json::as_str),
            Some(missing_fact),
            "breakpoint should name `{missing_fact}`: {breakpoint}"
        ),
        None => assert!(
            contract.get("blockedOn").is_none(),
            "breakpoint should not name a Marrow blocker for protocol validation: {breakpoint}"
        ),
    }
}

fn assert_invalid_breakpoint_line(breakpoint: &Json) {
    assert_eq!(breakpoint["verified"], false, "{breakpoint}");
    assert!(
        breakpoint.get("line").is_none(),
        "invalid breakpoint line should not be fabricated or echoed: {breakpoint}"
    );
    assert!(
        breakpoint.get("condition").is_none()
            && breakpoint.get("hitCondition").is_none()
            && breakpoint.get("logMessage").is_none(),
        "invalid breakpoint response must not echo request fields: {breakpoint}"
    );
    assert_breakpoint_contract(
        breakpoint,
        "dap.breakpoint.invalidLine",
        "invalid-params",
        None,
    );
}

fn assert_breakpoint_envelope_error(response: &Json, code: &str, status: &str) {
    assert_response_marrow_error(response, code, status, None);
    assert!(
        response["body"].get("breakpoints").is_none(),
        "failed setBreakpoints responses should not fabricate a breakpoint list: {response}"
    );
}

fn assert_evaluate_envelope_error(response: &Json, code: &str, status: &str) {
    assert_response_marrow_error(response, code, status, None);
    assert!(
        response["body"].get("result").is_none()
            && response["body"].get("variablesReference").is_none(),
        "failed evaluate responses should not fabricate an EvaluateResponse body: {response}"
    );
}

fn assert_variables_envelope_error(response: &Json, code: &str, status: &str) {
    assert_response_marrow_error(response, code, status, None);
    assert!(
        response["body"].get("variables").is_none(),
        "failed variables responses should not fabricate a variables list: {response}"
    );
}

fn assert_stack_trace_envelope_error(response: &Json, code: &str, status: &str) {
    assert_response_marrow_error(response, code, status, None);
    assert!(
        response["body"].get("stackFrames").is_none()
            && response["body"].get("totalFrames").is_none(),
        "failed stackTrace responses should not fabricate a stackTrace body: {response}"
    );
}

fn assert_scopes_envelope_error(response: &Json, code: &str, status: &str) {
    assert_response_marrow_error(response, code, status, None);
    assert!(
        response["body"].get("scopes").is_none(),
        "failed scopes responses should not fabricate a scopes list: {response}"
    );
}

fn assert_resume_envelope_error(response: &Json, code: &str, status: &str) {
    assert_response_marrow_error(response, code, status, None);
    assert!(
        response["body"].get("allThreadsContinued").is_none(),
        "failed resume responses should not fabricate a continue body: {response}"
    );
}

fn assert_arguments_envelope_error(response: &Json) {
    assert_response_marrow_error(response, "dap.arguments.invalid", "invalid-params", None);
    for field in [
        "breakpoints",
        "threads",
        "stackFrames",
        "totalFrames",
        "scopes",
        "variables",
        "result",
        "variablesReference",
        "allThreadsContinued",
    ] {
        assert!(
            response["body"].get(field).is_none(),
            "arguments-envelope failures should not fabricate `{field}`: {response}"
        );
    }
}

fn assert_launch_success(response: &Json) {
    assert_eq!(response["success"], true, "{response}");
    assert!(
        response["body"].get("marrowError").is_none(),
        "successful launch must not expose a blocked Marrow contract: {response}"
    );
}

fn assert_dap_execution_boundary(boundary: &Json, store_kind: &str, stamped: bool) {
    assert_eq!(boundary["sessionKind"], "run", "{boundary}");
    assert_eq!(
        boundary["sourceAnalysisGeneration"]["profileVersion"], "analysis.generation.v1",
        "{boundary}"
    );
    assert!(
        boundary["sourceAnalysisGeneration"]["sourceIdentity"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "{boundary}"
    );
    assert_eq!(boundary["store"]["kind"], store_kind, "{boundary}");
    if stamped {
        let stamp = &boundary["store"]["stamp"];
        assert!(
            stamp["store_uid"]
                .as_str()
                .is_some_and(|uid| uid.starts_with("store_")),
            "{boundary}"
        );
        assert!(stamp["catalog_epoch"].as_u64().is_some(), "{boundary}");
        assert!(stamp["commit_id"].as_u64().is_some(), "{boundary}");
    } else {
        assert_eq!(boundary["store"]["stamp"], Json::Null, "{boundary}");
    }
}

fn assert_dap_surface_serve_boundary(boundary: &Json) {
    assert_eq!(boundary["serveMode"], "write", "{boundary}");
    assert_eq!(
        boundary["dataViewBoundary"]["sourceAnalysisGeneration"]["profileVersion"],
        "analysis.generation.v1",
        "{boundary}"
    );
    assert_eq!(
        boundary["dataViewBoundary"]["compatibility"],
        json!({ "verdict": "admitted" }),
        "{boundary}"
    );
    assert_eq!(
        boundary["processControl"],
        json!({ "kind": "not_exposed" }),
        "{boundary}"
    );
    assert_eq!(
        boundary["dataViewBoundary"]["watchTargets"],
        json!([]),
        "isolated debug launch must not publish live store watch targets: {boundary}"
    );
}

#[test]
fn malformed_request_arguments_fail_before_command_handlers() {
    for (command, arguments) in [
        ("initialize", json!([])),
        ("launch", json!([])),
        ("setBreakpoints", json!([])),
        ("configurationDone", json!("bad")),
        ("threads", json!(false)),
        ("stackTrace", json!(1)),
        ("source", json!(1)),
        ("scopes", json!([])),
        ("variables", json!([])),
        ("evaluate", json!(true)),
        ("continue", json!([])),
        ("next", json!([])),
        ("stepIn", json!([])),
        ("stepOut", json!([])),
        ("pause", json!([])),
    ] {
        let mut client = Client::spawn();
        let request = client.request(command, arguments);
        let response = client.response_for(request);
        assert_arguments_envelope_error(&response);
    }

    for command in ["disconnect", "terminate"] {
        let mut client = Client::spawn();
        let request = client.request(command, json!([]));
        let response = client.response_for(request);
        assert_arguments_envelope_error(&response);

        let threads = client.request("threads", json!({}));
        let threads = client.response_for(threads);
        assert_eq!(threads["success"], true, "{threads}");
    }
}

#[test]
fn null_and_absent_request_arguments_remain_absent() {
    let mut client = Client::spawn();

    let init = client.request_without_arguments("initialize");
    assert_eq!(client.response_for(init)["success"], true);

    let threads = client.request("threads", Json::Null);
    let threads = client.response_for(threads);
    assert_eq!(threads["success"], true, "{threads}");
    assert_eq!(threads["body"]["threads"][0]["id"], 1, "{threads}");

    let done = client.request("configurationDone", Json::Null);
    let done = client.response_for(done);
    assert_response_marrow_error(&done, "dap.launch.notConfigured", "invalid-state", None);

    let launch = client.request("launch", Json::Null);
    let launch = client.response_for(launch);
    assert_response_marrow_error(&launch, "dap.launchProject.invalid", "invalid-params", None);
}

#[test]
fn unsupported_commands_do_not_parse_request_arguments() {
    for command in ["madeUpCommand", "attach"] {
        let mut client = Client::spawn();

        let request = client.request(command, json!([]));
        let response = client.response_for(request);
        assert_response_marrow_error(
            &response,
            "dap.unsupportedRequest",
            "unsupported-request",
            None,
        );
    }
}

#[test]
fn attach_is_not_supported_without_served_process_facts() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    let attach = client.request("attach", json!({ "project": "/tmp/project" }));
    let attach = client.response_for(attach);
    assert_response_marrow_error(
        &attach,
        "dap.unsupportedRequest",
        "unsupported-request",
        None,
    );

    let threads = client.request("threads", json!({}));
    let threads = client.response_for(threads);
    assert_eq!(
        threads["success"], true,
        "unsupported attach must leave the protocol session usable: {threads}"
    );
}

#[test]
fn terminate_without_a_running_session_reports_not_running() {
    let mut client = initialized_client();

    let terminate = client.request("terminate", json!({}));
    let terminate = client.response_for(terminate);
    assert_response_marrow_error(&terminate, "dap.notRunning", "invalid-state", None);

    let threads = client.request("threads", json!({}));
    let threads = client.response_for(threads);
    assert_eq!(threads["success"], true, "{threads}");
}

#[test]
fn prelaunch_breakpoint_inputs_return_invalid_state_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [
                { "line": 6 },
                { "line": 7, "condition": "title == \"Dune\"" },
                { "line": 8, "hitCondition": "3" },
                { "line": 9, "logMessage": "title changed" },
            ],
        }),
    );
    let response = client.response_for(set);
    assert_eq!(response["success"], true, "{response}");
    let breakpoints = response["body"]["breakpoints"].as_array().unwrap();
    assert_eq!(breakpoints.len(), 4, "{response}");

    assert_eq!(breakpoints[0]["verified"], false, "{response}");
    assert_eq!(breakpoints[0]["line"], 6, "{response}");
    assert_breakpoint_contract(
        &breakpoints[0],
        "dap.breakpoint.unverified",
        "invalid-state",
        None,
    );

    let condition = &breakpoints[1];
    assert_eq!(condition["verified"], false, "{response}");
    assert_eq!(condition["line"], 7, "{response}");
    assert_breakpoint_contract(
        condition,
        "dap.breakpoint.unverified",
        "invalid-state",
        None,
    );
    assert!(
        condition.get("condition").is_none()
            && condition.get("hitCondition").is_none()
            && condition.get("logMessage").is_none(),
        "pre-launch breakpoint response must not echo or retain expression inputs: {condition}"
    );

    for (index, line) in [(2, 8), (3, 9)] {
        let breakpoint = &breakpoints[index];
        assert_eq!(breakpoint["verified"], false, "{response}");
        assert_eq!(breakpoint["line"], line, "{response}");
        assert_breakpoint_contract(
            breakpoint,
            "dap.breakpoint.unverified",
            "invalid-state",
            None,
        );
        assert!(
            breakpoint.get("condition").is_none()
                && breakpoint.get("hitCondition").is_none()
                && breakpoint.get("logMessage").is_none(),
            "pre-launch breakpoint response must not echo or retain expression inputs: {breakpoint}"
        );
    }
}

#[test]
fn malformed_breakpoints_return_ordered_invalid_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [
                { "line": 6 },
                { "condition": "title == \"Dune\"" },
                { "line": "6" },
                { "line": -1 },
                { "line": 0 },
                { "line": 0, "condition": "title == \"Dune\"" },
                { "line": 4294967296u64 },
                { "line": 3.0 },
                { "line": 7, "condition": "title == \"Dune\"" },
            ],
        }),
    );
    let response = client.response_for(set);
    assert_eq!(response["success"], true, "{response}");
    let breakpoints = response["body"]["breakpoints"].as_array().unwrap();
    assert_eq!(breakpoints.len(), 9, "{response}");

    assert_eq!(breakpoints[0]["line"], 6, "{response}");
    assert_breakpoint_contract(
        &breakpoints[0],
        "dap.breakpoint.unverified",
        "invalid-state",
        None,
    );

    for breakpoint in &breakpoints[1..=7] {
        assert_invalid_breakpoint_line(breakpoint);
    }

    assert_eq!(breakpoints[8]["line"], 7, "{response}");
    assert_breakpoint_contract(
        &breakpoints[8],
        "dap.breakpoint.unverified",
        "invalid-state",
        None,
    );
}

#[test]
fn malformed_breakpoint_source_envelopes_fail_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    for source in [
        Json::Null,
        json!("not a source object"),
        json!({}),
        json!({ "sourceReference": 0 }),
        json!({ "sourceReference": -1 }),
        json!({ "sourceReference": 1.5 }),
        json!({ "path": 7 }),
    ] {
        let set = client.request(
            "setBreakpoints",
            json!({
                "source": source,
                "breakpoints": [{ "line": 6 }],
            }),
        );
        let response = client.response_for(set);
        assert_breakpoint_envelope_error(
            &response,
            "dap.breakpoint.sourceInvalid",
            "invalid-params",
        );
    }

    let clear = client.request(
        "setBreakpoints",
        json!({ "source": { "path": file.display().to_string() } }),
    );
    let response = client.response_for(clear);
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["body"]["breakpoints"], json!([]));
}

#[test]
fn malformed_source_reference_requests_fail_the_request() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    for arguments in [
        json!({}),
        json!({ "sourceReference": 0 }),
        json!({ "sourceReference": -1 }),
        json!({ "sourceReference": "1" }),
    ] {
        let request = client.request("source", arguments);
        let response = client.response_for(request);
        assert_response_marrow_error(
            &response,
            "dap.source.sourceReferenceInvalid",
            "invalid-params",
            None,
        );
    }
}

#[test]
fn unknown_source_reference_breakpoint_sources_are_rejected() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "sourceReference": 1 },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let response = client.response_for(set);
    assert_breakpoint_envelope_error(
        &response,
        "dap.breakpoint.sourceReferenceInvalid",
        "invalid-params",
    );
}

#[test]
fn positive_source_reference_takes_precedence_over_path() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": true,
        }),
    );
    assert_launch_success(&client.response_for(launch));
    client.event("initialized");

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    client.event("stopped");
    step_to_line(&mut client, 6);

    let threads = client.request("threads", json!({}));
    let thread_id = client.response_for(threads)["body"]["threads"][0]["id"]
        .as_i64()
        .unwrap();
    let stack = client.request("stackTrace", json!({ "threadId": thread_id }));
    let frames = client.response_for(stack);
    let source_reference = frames["body"]["stackFrames"][0]["source"]["sourceReference"]
        .as_i64()
        .expect("stack source exposes a reference");

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": {
                "path": "ignored.mw",
                "sourceReference": source_reference,
            },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let response = client.response_for(set);
    let breakpoints = response["body"]["breakpoints"].as_array().unwrap();
    assert_eq!(breakpoints.len(), 1, "{response}");
    assert_verified_breakpoint(&breakpoints[0], 6);

    let expected_source = file.canonicalize().unwrap();
    assert_eq!(
        frames["body"]["stackFrames"][0]["source"]["path"],
        expected_source.display().to_string(),
        "{frames}"
    );
}

#[test]
fn source_reference_returns_launch_snapshot_source() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": true,
        }),
    );
    assert_launch_success(&client.response_for(launch));
    client.event("initialized");

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    client.event("stopped");
    step_to_line(&mut client, 6);

    let threads = client.request("threads", json!({}));
    let thread_id = client.response_for(threads)["body"]["threads"][0]["id"]
        .as_i64()
        .unwrap();
    let stack = client.request("stackTrace", json!({ "threadId": thread_id }));
    let frames = client.response_for(stack);
    let source_reference = frames["body"]["stackFrames"][0]["source"]["sourceReference"]
        .as_i64()
        .filter(|reference| *reference > 0)
        .expect("stack source exposes a positive sourceReference");

    std::fs::write(
        &file,
        "module shelf\n\npub fn main()\n    print(\"changed\")\n",
    )
    .unwrap();

    let source = client.request("source", json!({ "sourceReference": source_reference }));
    let source = client.response_for(source);
    assert_eq!(source["success"], true, "{source}");
    assert_eq!(source["body"]["content"], SHELF_SOURCE, "{source}");
    assert_eq!(source["body"]["mimeType"], "text/x-marrow", "{source}");
}

#[test]
fn zero_source_reference_with_path_uses_the_path_source() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": {
                "path": file.display().to_string(),
                "sourceReference": 0,
            },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let response = client.response_for(set);
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["body"]["breakpoints"][0]["line"], 6, "{response}");
    assert_breakpoint_contract(
        &response["body"]["breakpoints"][0],
        "dap.breakpoint.unverified",
        "invalid-state",
        None,
    );
}

#[test]
fn non_array_breakpoints_fail_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": "line 6",
        }),
    );
    let response = client.response_for(set);
    assert_breakpoint_envelope_error(
        &response,
        "dap.breakpoint.breakpointsInvalid",
        "invalid-params",
    );
}

#[test]
fn non_object_breakpoints_stay_per_entry_invalid_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [42, "line 6"],
        }),
    );
    let response = client.response_for(set);
    assert_eq!(response["success"], true, "{response}");
    let breakpoints = response["body"]["breakpoints"].as_array().unwrap();
    assert_eq!(breakpoints.len(), 2, "{response}");
    for breakpoint in breakpoints {
        assert_invalid_breakpoint_line(breakpoint);
    }
}

#[test]
fn malformed_evaluate_expressions_fail_the_request() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    for arguments in [
        json!({}),
        json!({ "expression": 42 }),
        json!({ "expression": null }),
        json!({ "expression": { "value": "title" } }),
        json!({ "context": "hover" }),
        json!({ "context": "hover", "expression": 42 }),
    ] {
        let request = client.request("evaluate", arguments);
        let response = client.response_for(request);
        assert_evaluate_envelope_error(
            &response,
            "dap.evaluate.expressionInvalid",
            "invalid-params",
        );
    }
}

#[test]
fn malformed_evaluate_contexts_fail_the_request() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    for arguments in [
        json!({ "expression": "title", "context": 7 }),
        json!({ "expression": "^books(1).title", "context": null }),
        json!({ "expression": "title", "context": { "kind": "watch" } }),
    ] {
        let request = client.request("evaluate", arguments);
        let response = client.response_for(request);
        assert_evaluate_envelope_error(&response, "dap.evaluate.contextInvalid", "invalid-params");
    }
}

#[test]
fn valid_evaluate_envelopes_keep_typed_contracts() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    let unknown_context = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "clipboard" }),
    );
    let unknown_context = client.response_for(unknown_context);
    assert_evaluate_envelope_error(
        &unknown_context,
        "dap.evaluate.contextInvalid",
        "invalid-params",
    );

    let whitespace = client.request(
        "evaluate",
        json!({ "expression": "   ", "context": "watch" }),
    );
    let whitespace = client.response_for(whitespace);
    assert_evaluate_envelope_error(
        &whitespace,
        "dap.evaluate.expressionInvalid",
        "invalid-params",
    );

    let hover = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "hover" }),
    );
    let hover = client.response_for(hover);
    assert_response_marrow_error(&hover, "dap.notStopped", "invalid-state", None);

    let durable = client.request(
        "evaluate",
        json!({ "expression": "^books(1).title", "context": "watch" }),
    );
    let durable = client.response_for(durable);
    assert_response_marrow_error(&durable, "dap.notStopped", "invalid-state", None);
}

#[test]
fn malformed_variables_references_fail_the_request() {
    let (_dir, mut client) = stopped_fixture_client();
    let cases = [
        json!({}),
        json!({ "variablesReference": null }),
        json!({ "variablesReference": "2" }),
        json!({ "variablesReference": 2.0 }),
        json!({ "variablesReference": 0 }),
        json!({ "variablesReference": -1 }),
    ];

    for arguments in cases {
        let request = client.request("variables", arguments);
        let response = client.response_for(request);
        assert_variables_envelope_error(
            &response,
            "dap.variables.referenceInvalid",
            "invalid-params",
        );
    }
}

#[test]
fn malformed_variables_optional_arguments_fail_the_request() {
    let (_dir, mut client) = stopped_fixture_client();

    for arguments in [
        json!({ "variablesReference": 1, "start": "0" }),
        json!({ "variablesReference": 1, "start": -1 }),
        json!({ "variablesReference": 1, "start": 1.0 }),
        json!({ "variablesReference": 1, "count": "1" }),
        json!({ "variablesReference": 1, "count": -1 }),
        json!({ "variablesReference": 1, "count": 1.0 }),
        json!({ "variablesReference": 2, "start": "0" }),
    ] {
        let request = client.request("variables", arguments);
        let response = client.response_for(request);
        assert_variables_envelope_error(&response, "dap.variables.pageInvalid", "invalid-params");
    }

    for arguments in [
        json!({ "variablesReference": 1, "filter": "all" }),
        json!({ "variablesReference": 1, "filter": 1 }),
    ] {
        let request = client.request("variables", arguments);
        let response = client.response_for(request);
        assert_variables_envelope_error(&response, "dap.variables.filterInvalid", "invalid-params");
    }
}

#[test]
fn variables_optional_null_and_paging_off_arguments_are_accepted() {
    let (_dir, mut client) = stopped_fixture_client();
    step_to_line(&mut client, 6);

    let baseline = client.request("variables", json!({ "variablesReference": 1 }));
    let baseline = client.response_for(baseline);
    assert_eq!(baseline["success"], true, "{baseline}");
    let baseline_variables = baseline["body"]["variables"].clone();
    assert!(
        !baseline_variables.as_array().unwrap().is_empty(),
        "paging-off checks should compare against real local variables: {baseline}"
    );

    for arguments in [
        json!({ "variablesReference": 1, "start": null, "count": null, "filter": null }),
        json!({ "variablesReference": 1, "start": 1, "count": 1 }),
        json!({ "variablesReference": 1, "filter": "named" }),
    ] {
        let request = client.request("variables", arguments);
        let response = client.response_for(request);
        assert_eq!(response["success"], true, "{response}");
        assert_eq!(
            response["body"]["variables"], baseline_variables,
            "{response}"
        );
    }
}

#[test]
fn unknown_positive_variables_reference_stays_empty() {
    let (_dir, mut client) = stopped_fixture_client();

    let request = client.request("variables", json!({ "variablesReference": 99999 }));
    let response = client.response_for(request);
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["body"]["variables"], json!([]), "{response}");
    assert!(
        response["body"].get("marrowError").is_none(),
        "unknown positive variables references should remain stale-reference tolerant: {response}"
    );
}

#[test]
fn malformed_stack_trace_thread_ids_fail_the_request() {
    let mut client = initialized_client();
    let cases = [
        json!({}),
        json!({ "threadId": null }),
        json!({ "threadId": "1" }),
        json!({ "threadId": 1.0 }),
        json!({ "threadId": 0 }),
        json!({ "threadId": -1 }),
        json!({ "threadId": 2 }),
    ];

    for arguments in cases {
        let request = client.request("stackTrace", arguments);
        let response = client.response_for(request);
        assert_stack_trace_envelope_error(&response, "dap.threadId.invalid", "invalid-params");
    }
}

#[test]
fn stack_trace_with_valid_thread_id_stays_empty_without_a_stopped_frame() {
    let mut client = initialized_client();

    let request = client.request("stackTrace", json!({ "threadId": 1 }));
    let response = client.response_for(request);
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["body"]["stackFrames"], json!([]), "{response}");
    assert_eq!(response["body"]["totalFrames"], 0, "{response}");
    assert!(
        response["body"].get("marrowError").is_none(),
        "successful empty stackTrace should not carry an error contract: {response}"
    );
}

#[test]
fn malformed_scopes_frame_ids_fail_the_request() {
    let mut client = initialized_client();
    let cases = [
        json!({}),
        json!({ "frameId": null }),
        json!({ "frameId": "1" }),
        json!({ "frameId": 1.0 }),
        json!({ "frameId": 0 }),
        json!({ "frameId": -1 }),
        json!({ "frameId": 2 }),
    ];

    for arguments in cases {
        let request = client.request("scopes", arguments);
        let response = client.response_for(request);
        assert_scopes_envelope_error(&response, "dap.frameId.invalid", "invalid-params");
    }
}

#[test]
fn scopes_require_a_current_stopped_frame() {
    let mut client = initialized_client();

    let request = client.request("scopes", json!({ "frameId": 1 }));
    let response = client.response_for(request);
    assert_scopes_envelope_error(&response, "dap.notStopped", "invalid-state");
}

#[test]
fn malformed_resume_thread_ids_fail_before_state_checks() {
    let mut client = initialized_client();

    for command in ["continue", "next", "stepIn", "stepOut", "pause"] {
        for arguments in [json!({}), json!({ "threadId": "1" })] {
            let request = client.request(command, arguments);
            let response = client.response_for(request);
            assert_resume_envelope_error(&response, "dap.threadId.invalid", "invalid-params");
        }
    }
}

#[test]
fn launch_accepts_explicit_entry_with_typed_args() {
    let dir = tempfile::tempdir().unwrap();
    write_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "entry": "shelf::main",
            "args": [
                { "name": "title", "value": { "kind": "string", "value": "Dune" } }
            ],
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);

    let output = client.event("output");
    assert!(
        output["body"]["output"]
            .as_str()
            .is_some_and(|output| output.contains("Dune")),
        "typed launch arg should reach the entry: {output}"
    );
    client.event("terminated");
}

#[test]
fn configuration_done_rejects_source_changed_after_launch() {
    let dir = tempfile::tempdir().unwrap();
    write_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "entry": "shelf::main",
            "args": [
                { "name": "title", "value": { "kind": "string", "value": "Dune" } }
            ],
        }),
    );
    assert_launch_success(&client.response_for(launch));

    std::fs::write(
        dir.path().join("src").join("shelf.mw"),
        "module shelf\n\npub fn default()\n    print(\"default\")\n\npub fn main(title: string)\n    print(\"configurationDone-version\")\n",
    )
    .unwrap();

    let done = client.request("configurationDone", json!({}));
    assert_response_marrow_error(
        &client.response_for(done),
        "dap.launchAnalysis.changed",
        "invalid-state",
        None,
    );

    let source = client.request("source", json!({ "sourceReference": 1 }));
    assert_response_marrow_error(
        &client.response_for(source),
        "dap.source.sourceReferenceInvalid",
        "invalid-params",
        None,
    );

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "sourceReference": 1 },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    assert_breakpoint_envelope_error(
        &client.response_for(set),
        "dap.breakpoint.sourceReferenceInvalid",
        "invalid-params",
    );
}

#[test]
fn configuration_done_rejects_catalog_generation_changed_after_launch() {
    let dir = tempfile::tempdir().unwrap();
    write_native_identity_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
        }),
    );
    assert_launch_success(&client.response_for(launch));

    commit_proposed_catalog_lock(dir.path());

    let done = client.request("configurationDone", json!({}));
    assert_response_marrow_error(
        &client.response_for(done),
        "dap.launchAnalysis.changed",
        "invalid-state",
        None,
    );
}

#[test]
fn launch_rejects_non_array_args_as_protocol_validation() {
    let dir = tempfile::tempdir().unwrap();
    write_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "args": "Dune",
        }),
    );
    let rejected = client.response_for(launch);
    assert_response_marrow_error(
        &rejected,
        "dap.launchArgs.invalidType",
        "invalid-params",
        None,
    );
    assert!(
        rejected["message"]
            .as_str()
            .is_some_and(|message| message.contains("`args` must be an array")),
        "non-array args should fail at the DAP array-shape boundary: {rejected}"
    );
}

#[test]
fn launch_rejects_malformed_typed_args_as_protocol_validation() {
    let dir = tempfile::tempdir().unwrap();
    write_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "args": [1],
        }),
    );
    let rejected = client.response_for(launch);
    assert_response_marrow_error(
        &rejected,
        "dap.launchArgs.invalidType",
        "invalid-params",
        None,
    );
    assert!(
        rejected["message"]
            .as_str()
            .is_some_and(|message| message.contains("run argument 0 must be an object")),
        "malformed launch args should preserve Marrow admission details: {rejected}"
    );
}

#[test]
fn launch_rejects_numeric_typed_int_args_as_protocol_validation() {
    let dir = tempfile::tempdir().unwrap();
    write_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "entry": "shelf::main",
            "args": [
                { "name": "title", "value": { "kind": "int", "value": 1 } }
            ],
        }),
    );
    let rejected = client.response_for(launch);
    assert_response_marrow_error(
        &rejected,
        "dap.launchArgs.invalidType",
        "invalid-params",
        None,
    );
    assert!(
        rejected["message"]
            .as_str()
            .is_some_and(|message| message.contains("needs string field `value`")),
        "numeric int launch args should report Marrow's exact admission detail: {rejected}"
    );
}

#[test]
fn launch_rejects_concrete_non_string_entry_as_protocol_validation() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "entry": 42,
        }),
    );
    let rejected = client.response_for(launch);
    assert_response_marrow_error(
        &rejected,
        "dap.launchEntry.invalidType",
        "invalid-params",
        None,
    );
}

#[test]
fn launch_treats_null_entry_as_absent() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "entry": null,
        }),
    );
    let response = client.response_for(launch);
    assert_launch_success(&response);
}

#[test]
fn launch_rejects_concrete_non_boolean_stop_on_entry_as_protocol_validation() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": "yes",
        }),
    );
    let rejected = client.response_for(launch);
    assert_response_marrow_error(
        &rejected,
        "dap.launchStopOnEntry.invalidType",
        "invalid-params",
        None,
    );
}

#[test]
fn launch_treats_null_stop_on_entry_as_absent() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": null,
        }),
    );
    let response = client.response_for(launch);
    assert_launch_success(&response);
}

#[test]
fn launch_rejects_blank_project_as_protocol_validation() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request("launch", json!({ "project": "   " }));
    let rejected = client.response_for(launch);
    assert_response_marrow_error(
        &rejected,
        "dap.launchProject.invalid",
        "invalid-params",
        None,
    );
}

#[test]
fn malformed_launch_does_not_arm_configuration_done() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": 1,
        }),
    );
    let rejected = client.response_for(launch);
    assert_response_marrow_error(
        &rejected,
        "dap.launchStopOnEntry.invalidType",
        "invalid-params",
        None,
    );

    let done = client.request("configurationDone", json!({}));
    let rejected = client.response_for(done);
    assert_response_marrow_error(&rejected, "dap.launch.notConfigured", "invalid-state", None);
}

#[test]
fn launch_rejects_missing_project_config_with_typed_contract() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
        }),
    );
    let rejected = client.response_for(launch);
    assert_response_marrow_error(
        &rejected,
        "dap.launchProject.invalid",
        "invalid-project",
        None,
    );
}

#[test]
fn launch_accepts_configured_default_entry_with_isolated_writes() {
    let dir = tempfile::tempdir().unwrap();
    write_native_identity_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
        }),
    );
    let response = client.response_for(launch);
    assert_launch_success(&response);
    assert_dap_execution_boundary(
        &response["body"]["marrowDebug"]["previewExecutionBoundary"],
        "plain_memory",
        false,
    );
    assert_dap_surface_serve_boundary(
        &response["body"]["marrowDebug"]["previewSurfaceServeBoundary"],
    );

    let done = client.request("configurationDone", json!({}));
    let done = client.response_for(done);
    assert_eq!(done["success"], true, "{done}");
    assert_dap_execution_boundary(
        &done["body"]["marrowDebug"]["executionBoundary"],
        "plain_memory",
        false,
    );
    assert_dap_surface_serve_boundary(&done["body"]["marrowDebug"]["surfaceServeBoundary"]);

    let output = client.event("output");
    assert!(
        output["body"]["output"]
            .as_str()
            .unwrap()
            .contains("seeded"),
        "configured entry should run through the session path: {output}"
    );
    client.event("terminated");

    assert!(
        !dir.path().join(CATALOG_FILE_NAME).exists(),
        "isolated DAP launch must not write the committed lock"
    );
    assert!(
        !dir.path().join("data").join("marrow.redb").exists(),
        "isolated DAP launch must not open or create the native store"
    );
}

#[test]
fn launch_accepts_valid_existing_native_store() {
    let dir = tempfile::tempdir().unwrap();
    write_native_identity_fixture(dir.path());
    seed_native_store(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
        }),
    );
    let response = client.response_for(launch);
    assert_launch_success(&response);
    assert_dap_execution_boundary(
        &response["body"]["marrowDebug"]["previewExecutionBoundary"],
        "isolated",
        true,
    );
    assert_dap_surface_serve_boundary(
        &response["body"]["marrowDebug"]["previewSurfaceServeBoundary"],
    );

    let done = client.request("configurationDone", json!({}));
    let done = client.response_for(done);
    assert_eq!(done["success"], true, "{done}");
    assert_dap_execution_boundary(
        &done["body"]["marrowDebug"]["executionBoundary"],
        "isolated",
        true,
    );
    assert_dap_surface_serve_boundary(&done["body"]["marrowDebug"]["surfaceServeBoundary"]);
    client.event("output");
    client.event("terminated");
}

#[test]
fn launch_rejects_invalid_utf8_read_admission_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    write_native_identity_fixture(dir.path());
    corrupt_catalog_utf8_with_empty_native_store(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
        }),
    );
    let rejected = client.response_for(launch);
    assert_response_marrow_error(
        &rejected,
        "dap.launchReadAdmission.invalid",
        "invalid-project",
        None,
    );
    assert_eq!(
        std::fs::read(dir.path().join(marrow_project::CATALOG_FILE_NAME)).unwrap(),
        [0xff],
        "DAP launch must not mutate the invalid committed lock"
    );
}

#[test]
fn launch_accepts_explicit_zero_arg_entry() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "entry": "shelf::main",
        }),
    );
    assert_launch_success(&client.response_for(launch));
}

#[test]
fn verified_breakpoint_and_stepped_stop_exposes_locals() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = Client::spawn();

    // initialize -> response.
    let init = client.request("initialize", json!({}));
    let response = client.response_for(init);
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(
        response["body"]["supportsConfigurationDoneRequest"], true,
        "{response}"
    );
    assert!(
        response["body"].get("supportsVariablePaging").is_none(),
        "{response}"
    );

    // launch the fixture's default entry.
    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": true,
        }),
    );
    assert_eq!(client.response_for(launch)["success"], true);
    client.event("initialized");

    // The print statement is line 6; Marrow stop-point facts verify the line.
    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let response = client.response_for(set);
    let breakpoints = response["body"]["breakpoints"].as_array().unwrap();
    assert_eq!(breakpoints.len(), 1, "{response}");
    assert_verified_breakpoint(&breakpoints[0], 6);

    // configurationDone starts at the entry stop; stepping reaches the inspection line.
    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");
    step_to_line(&mut client, 6);

    // threads + stackTrace + scopes.
    let threads = client.request("threads", json!({}));
    let thread_id = client.response_for(threads)["body"]["threads"][0]["id"]
        .as_i64()
        .unwrap();
    let stack = client.request("stackTrace", json!({ "threadId": thread_id }));
    let frames = client.response_for(stack);
    let frame = &frames["body"]["stackFrames"][0];
    assert_eq!(frame["line"], 6, "{frames}");
    let expected_source = file.canonicalize().unwrap();
    assert_eq!(
        frame["source"]["path"],
        expected_source.display().to_string(),
        "stack source should use the stopped frame's Marrow-owned file path: {frames}"
    );
    let frame_id = frame["id"].as_i64().unwrap();

    let scopes = client.request("scopes", json!({ "frameId": frame_id }));
    let scopes = client.response_for(scopes);
    let scope_list = scopes["body"]["scopes"].as_array().unwrap();
    let locals_ref = scope_list
        .iter()
        .find(|scope| scope["name"] == "Locals")
        .and_then(|scope| scope["variablesReference"].as_i64())
        .expect("a Locals scope");
    let durable_ref = scope_list
        .iter()
        .find(|scope| scope["name"] == "Durable Data")
        .and_then(|scope| scope["variablesReference"].as_i64())
        .expect("a Durable Data scope");

    // The Locals scope exposes `title = "Dune"` (a string local in scope).
    let locals = client.request("variables", json!({ "variablesReference": locals_ref }));
    let locals = client.response_for(locals);
    let local_vars = locals["body"]["variables"].as_array().unwrap();
    assert_eq!(local_vars.len(), 2, "{locals}");
    let title = local_vars
        .iter()
        .find(|variable| variable["name"] == "title")
        .expect("a `title` local");
    assert_eq!(title["value"], "Dune", "{locals}");
    assert_runtime_debug_value(title);

    let title = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "watch" }),
    );
    let title = client.response_for(title);
    assert_evaluate_runtime_debug_value(&title, "Dune", 0);

    let hover_title = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "hover" }),
    );
    let hover_title = client.response_for(hover_title);
    assert_evaluate_runtime_debug_value(&hover_title, "Dune", 0);

    let addition = client.request("evaluate", json!({ "expression": "1 + 1" }));
    let addition = client.response_for(addition);
    assert_evaluate_runtime_debug_value(&addition, "2", 0);

    let invalid = client.request(
        "evaluate",
        json!({ "expression": "_x1", "context": "repl" }),
    );
    let invalid = client.response_for(invalid);
    assert_evaluate_envelope_error(&invalid, "dap.evaluate.expressionInvalid", "invalid-params");

    let durable_variables =
        client.request("variables", json!({ "variablesReference": durable_ref }));
    let durable_variables = client.response_for(durable_variables);
    assert_eq!(
        durable_variables["body"]["variables"],
        json!([]),
        "a project with no saved roots has an empty Durable Data scope: {durable_variables}"
    );

    let watch = client.request(
        "evaluate",
        json!({ "expression": "^books(1).title", "context": "watch" }),
    );
    let watch = client.response_for(watch);
    assert_evaluate_envelope_error(&watch, "dap.evaluate.expressionInvalid", "invalid-params");

    let hover_watch = client.request(
        "evaluate",
        json!({ "expression": "^books(1).title", "context": "hover" }),
    );
    let hover_watch = client.response_for(hover_watch);
    assert_evaluate_envelope_error(
        &hover_watch,
        "dap.evaluate.expressionInvalid",
        "invalid-params",
    );

    // continue to termination: expect the print output and a terminated event.
    let cont = client.request("continue", json!({ "threadId": thread_id }));
    assert_eq!(client.response_for(cont)["success"], true);

    let output = client.event("output");
    assert!(
        output["body"]["output"].as_str().unwrap().contains("Dune"),
        "{output}"
    );
    client.event("terminated");
}

#[test]
fn pipelined_hover_after_continue_reports_not_stopped() {
    let (_dir, _file, mut client) = entry_stopped_client_with_breakpoint(6);

    let cont = client.request("continue", json!({ "threadId": 1 }));
    let hover = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "hover" }),
    );

    assert_eq!(client.response_for(cont)["success"], true);
    let hover = client.response_for(hover);
    assert_response_marrow_error(&hover, "dap.notStopped", "invalid-state", None);
}

#[test]
fn pipelined_double_continue_rejects_second_resume() {
    let (_dir, _file, mut client) = entry_stopped_client_with_breakpoint(6);

    let first = client.request("continue", json!({ "threadId": 1 }));
    let second = client.request("continue", json!({ "threadId": 1 }));

    assert_eq!(client.response_for(first)["success"], true);
    let second = client.response_for(second);
    assert_response_marrow_error(&second, "dap.notStopped", "invalid-state", None);

    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");
    assert_eq!(stop_line(&mut client), 6);

    let hover = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "hover" }),
    );
    let hover = client.response_for(hover);
    assert_eq!(hover["success"], true, "{hover}");
    assert_eq!(hover["body"]["result"], "Dune", "{hover}");
}

#[test]
fn pause_during_continue_stops_at_the_next_statement() {
    let (_dir, _file, mut client) = entry_stopped_client_with_breakpoint(6);

    let cont = client.request("continue", json!({ "threadId": 1 }));
    let pause = client.request("pause", json!({ "threadId": 1 }));

    assert_eq!(client.response_for(cont)["success"], true);
    assert_eq!(client.response_for(pause)["success"], true);

    // The async pause flag is serviced at the next statement boundary, ahead of
    // the line-6 breakpoint, so the run stops with a `pause` reason.
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "pause", "{stopped}");
}

#[test]
fn breakpoint_added_during_continue_takes_effect_on_the_next_stop() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": true,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");

    let cont = client.request("continue", json!({ "threadId": 1 }));
    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6 }],
        }),
    );

    assert_eq!(client.response_for(cont)["success"], true);
    let set = client.response_for(set);
    assert_eq!(set["success"], true, "{set}");
    assert_verified_breakpoint(&set["body"]["breakpoints"][0], 6);

    // The breakpoint pushed mid-continue is picked up outside park and arms the
    // next stop.
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");
    assert_eq!(stop_line(&mut client), 6);
}

#[test]
fn breakpoint_removed_during_continue_takes_effect() {
    let (_dir, file, mut client) = entry_stopped_client_with_breakpoint(6);

    let cont = client.request("continue", json!({ "threadId": 1 }));
    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [],
        }),
    );

    assert_eq!(client.response_for(cont)["success"], true);
    assert_eq!(client.response_for(set)["success"], true);

    // Clearing the breakpoint mid-continue disarms line 6, so the run finishes
    // without a stop.
    assert_terminates_without_stopping(&mut client);
}

#[test]
fn terminate_while_running_suppresses_later_stop_events() {
    let (_dir, _file, mut client) = entry_stopped_client_with_breakpoint(6);

    let cont = client.request("continue", json!({ "threadId": 1 }));
    let terminate = client.request("terminate", json!({}));

    assert_eq!(client.response_for(cont)["success"], true);
    assert_eq!(client.response_for(terminate)["success"], true);

    for _ in 0..100 {
        let message = client.read();
        assert_ne!(
            message["event"], "stopped",
            "terminating while running must not publish a stale stopped frame: {message}"
        );
        if message["type"] == "event" && message["event"] == "terminated" {
            return;
        }
    }
    panic!("terminate did not end the session within the bounded message loop");
}

/// Count the isolated-store scratch directories a given adapter process holds.
/// The run copies the native store into `marrow-dry-run-store-<pid>-…` and the
/// owning `ProjectSession` removes it on Drop, so a leftover dir marks a teardown
/// that never ran.
fn isolated_store_dir_count(pid: u32) -> usize {
    let prefix = format!("marrow-dry-run-store-{pid}-");
    std::fs::read_dir(std::env::temp_dir())
        .expect("read the system temp dir")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .count()
}

#[test]
fn dropping_input_stream_mid_run_tears_down_the_project_session() {
    let dir = tempfile::tempdir().unwrap();
    write_native_identity_fixture(dir.path());
    seed_native_store(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({ "project": dir.path().display().to_string(), "stopOnEntry": true }),
    );
    assert_launch_success(&client.response_for(launch));

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");

    let pid = client.child_id();
    assert!(
        isolated_store_dir_count(pid) > 0,
        "the stopped run should hold an isolated-store scratch dir"
    );

    // Closing input mid-run reaches serve()'s disconnect path. Guaranteed teardown
    // must still run terminate_session so the run thread unwinds and the
    // ProjectSession reclaims its scratch dir rather than being orphaned.
    client.close_input_and_wait();
    assert_eq!(
        isolated_store_dir_count(pid),
        0,
        "an EOF disconnect must not orphan the run thread's ProjectSession"
    );
}

#[test]
fn verified_breakpoint_stops_without_stop_on_entry() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let response = client.response_for(set);
    assert_verified_breakpoint(&response["body"]["breakpoints"][0], 6);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");
    assert_eq!(stop_line(&mut client), 6);

    let cont = client.request("continue", json!({ "threadId": 1 }));
    assert_eq!(client.response_for(cont)["success"], true);
    client.event("terminated");
}

#[test]
fn prelaunch_breakpoint_remains_unarmed_after_launch_prepares_stop_points() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let response = client.response_for(set);
    assert_breakpoint_contract(
        &response["body"]["breakpoints"][0],
        "dap.breakpoint.unverified",
        "invalid-state",
        None,
    );

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    assert_terminates_without_stopping(&mut client);
}

#[test]
fn absolute_breakpoint_verifies_when_launch_project_is_relative() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = Client::spawn_in(dir.path());

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);

    let launch = client.request(
        "launch",
        json!({
            "project": ".",
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let response = client.response_for(set);
    assert_verified_breakpoint(&response["body"]["breakpoints"][0], 6);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");
    assert_eq!(stop_line(&mut client), 6);

    let cont = client.request("continue", json!({ "threadId": 1 }));
    assert_eq!(client.response_for(cont)["success"], true);
    client.event("terminated");
}

#[cfg(unix)]
#[test]
fn breakpoint_set_on_real_path_arms_symlinked_runtime_source_root() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_symlink_source_root_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let response = client.response_for(set);
    assert_verified_breakpoint(&response["body"]["breakpoints"][0], 6);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");
    let stack = client.request("stackTrace", json!({ "threadId": 1 }));
    let stack = client.response_for(stack);
    let frame = &stack["body"]["stackFrames"][0];
    assert_eq!(frame["line"], 6, "{stack}");
    let source_reference = frame["source"]["sourceReference"]
        .as_i64()
        .filter(|reference| *reference > 0)
        .expect("symlinked source frame exposes a positive sourceReference");
    let source = client.request("source", json!({ "sourceReference": source_reference }));
    let source = client.response_for(source);
    assert_eq!(source["success"], true, "{source}");
    assert_eq!(source["body"]["content"], SHELF_SOURCE, "{source}");

    let cont = client.request("continue", json!({ "threadId": 1 }));
    assert_eq!(client.response_for(cont)["success"], true);
    client.event("terminated");
}

#[test]
fn breakpoint_set_while_stopped_is_armed_on_continue() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": true,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");
    assert_eq!(stop_line(&mut client), 4);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let response = client.response_for(set);
    assert_verified_breakpoint(&response["body"]["breakpoints"][0], 6);

    let cont = client.request("continue", json!({ "threadId": 1 }));
    assert_eq!(client.response_for(cont)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");
    assert_eq!(stop_line(&mut client), 6);

    let cont = client.request("continue", json!({ "threadId": 1 }));
    assert_eq!(client.response_for(cont)["success"], true);
    client.event("terminated");
}

#[test]
fn relaunch_while_stopped_does_not_clear_running_breakpoints() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": true,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let response = client.response_for(set);
    assert_verified_breakpoint(&response["body"]["breakpoints"][0], 6);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");

    let relaunch = client.request("launch", json!({ "project": "" }));
    let rejected = client.response_for(relaunch);
    assert_response_marrow_error(
        &rejected,
        "dap.launch.alreadyConfigured",
        "invalid-state",
        None,
    );

    let clear = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [],
        }),
    );
    let response = client.response_for(clear);
    assert_eq!(response["body"]["breakpoints"], json!([]));

    let cont = client.request("continue", json!({ "threadId": 1 }));
    assert_eq!(client.response_for(cont)["success"], true);
    assert_terminates_without_stopping(&mut client);
}

#[test]
fn non_stop_point_breakpoint_is_not_armed_after_launch() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 3 }],
        }),
    );
    let response = client.response_for(set);
    let breakpoint = &response["body"]["breakpoints"][0];
    assert_eq!(breakpoint["verified"], false, "{response}");
    assert_eq!(breakpoint["line"], 3, "{response}");
    assert_breakpoint_contract(
        breakpoint,
        "dap.breakpoint.noStopPoint",
        "invalid-params",
        None,
    );

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    assert_terminates_without_stopping(&mut client);
}

#[test]
fn true_conditional_breakpoint_stops_without_stop_on_entry() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6, "condition": "title == \"Dune\"" }],
        }),
    );
    let response = client.response_for(set);
    assert_verified_breakpoint(&response["body"]["breakpoints"][0], 6);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");
    assert_eq!(stop_line(&mut client), 6);

    let cont = client.request("continue", json!({ "threadId": 1 }));
    assert_eq!(client.response_for(cont)["success"], true);
    client.event("terminated");
}

#[test]
fn false_conditional_breakpoint_verifies_but_does_not_stop() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6, "condition": "title == \"Other\"" }],
        }),
    );
    let response = client.response_for(set);
    assert_verified_breakpoint(&response["body"]["breakpoints"][0], 6);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    assert_terminates_without_stopping(&mut client);
}

#[test]
fn durable_conditional_breakpoint_evaluates_against_frame_data() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_durable_debug_data_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 9, "condition": "(^books(id).title ?? \"\") == \"Dune\"" }],
        }),
    );
    let response = client.response_for(set);
    let breakpoint = &response["body"]["breakpoints"][0];
    assert_verified_breakpoint(breakpoint, 9);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");
}

#[test]
fn hit_condition_stops_on_the_requested_encounter() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_repeated_call_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 4, "hitCondition": "2" }],
        }),
    );
    let response = client.response_for(set);
    assert_verified_breakpoint(&response["body"]["breakpoints"][0], 4);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");
    assert_eq!(stop_line(&mut client), 4);

    let cont = client.request("continue", json!({ "threadId": 1 }));
    assert_eq!(client.response_for(cont)["success"], true);
    client.event("terminated");
}

#[test]
fn logpoint_emits_output_without_stopping() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6, "logMessage": "title changed" }],
        }),
    );
    let response = client.response_for(set);
    assert_verified_breakpoint(&response["body"]["breakpoints"][0], 6);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let output = client.event("output");
    assert_eq!(output["body"]["category"], "console", "{output}");
    assert_eq!(output["body"]["output"], "title changed\n", "{output}");
    client.event("terminated");
}

#[test]
fn logpoint_expression_interpolation_evaluates_against_frame_data() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6, "logMessage": "title {title}" }],
        }),
    );
    let response = client.response_for(set);
    assert_verified_breakpoint(&response["body"]["breakpoints"][0], 6);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let output = client.event("output");
    assert_eq!(output["body"]["category"], "console", "{output}");
    assert_eq!(output["body"]["output"], "title Dune\n", "{output}");
    client.event("terminated");
}

#[test]
fn invalid_logpoint_expression_is_not_armed() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6, "logMessage": "title {missing}" }],
        }),
    );
    let response = client.response_for(set);
    let breakpoint = &response["body"]["breakpoints"][0];
    assert_eq!(breakpoint["verified"], false, "{response}");
    assert_eq!(breakpoint["line"], 6, "{response}");
    assert_breakpoint_contract(
        breakpoint,
        "dap.breakpoint.logMessageInvalid",
        "invalid-params",
        None,
    );

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    assert_terminates_without_stopping(&mut client);
}

#[test]
fn malformed_logpoint_template_is_not_armed() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6, "logMessage": "bad } {title}" }],
        }),
    );
    let response = client.response_for(set);
    let breakpoint = &response["body"]["breakpoints"][0];
    assert_eq!(breakpoint["verified"], false, "{response}");
    assert_eq!(breakpoint["line"], 6, "{response}");
    assert_breakpoint_contract(
        breakpoint,
        "dap.breakpoint.logMessageInvalid",
        "invalid-params",
        None,
    );

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    assert_terminates_without_stopping(&mut client);
}

#[test]
fn malformed_hit_conditions_are_rejected_and_unarmed() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [
                { "line": 6, "hitCondition": "0" },
                { "line": 6, "hitCondition": "-1" },
                { "line": 6, "hitCondition": "+1" },
                { "line": 6, "hitCondition": " 1 " },
                { "line": 6, "hitCondition": "   " },
            ],
        }),
    );
    let response = client.response_for(set);
    let breakpoints = response["body"]["breakpoints"].as_array().unwrap();
    assert_eq!(breakpoints.len(), 5, "{response}");
    for breakpoint in breakpoints {
        assert_eq!(breakpoint["verified"], false, "{response}");
        assert_eq!(breakpoint["line"], 6, "{response}");
        assert_breakpoint_contract(
            breakpoint,
            "dap.breakpoint.hitConditionInvalid",
            "invalid-params",
            None,
        );
    }

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    assert_terminates_without_stopping(&mut client);
}

#[test]
fn empty_logpoint_messages_are_rejected_and_unarmed() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6, "logMessage": "" }],
        }),
    );
    let response = client.response_for(set);
    let breakpoint = &response["body"]["breakpoints"][0];
    assert_eq!(breakpoint["verified"], false, "{response}");
    assert_eq!(breakpoint["line"], 6, "{response}");
    assert_breakpoint_contract(
        breakpoint,
        "dap.breakpoint.logMessageInvalid",
        "invalid-params",
        None,
    );

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    assert_terminates_without_stopping(&mut client);
}

#[test]
fn invalid_conditional_breakpoints_are_not_armed() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [
                { "line": 6, "condition": "1" },
                { "line": 6, "condition": "missing_name" },
            ],
        }),
    );
    let response = client.response_for(set);
    let breakpoints = response["body"]["breakpoints"].as_array().unwrap();
    assert_eq!(breakpoints.len(), 2, "{response}");
    for breakpoint in breakpoints {
        assert_eq!(breakpoint["verified"], false, "{response}");
        assert_eq!(breakpoint["line"], 6, "{response}");
        assert_breakpoint_contract(
            breakpoint,
            "dap.breakpoint.conditionInvalid",
            "invalid-params",
            None,
        );
    }

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    assert_terminates_without_stopping(&mut client);
}

#[test]
fn non_string_breakpoint_expression_fields_are_rejected_and_unarmed() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = initialized_client();

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": false,
        }),
    );
    assert_launch_success(&client.response_for(launch));

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [
                { "line": 6, "condition": true },
                { "line": 6, "hitCondition": 3 },
                { "line": 6, "logMessage": { "text": "title changed" } },
            ],
        }),
    );
    let response = client.response_for(set);
    let breakpoints = response["body"]["breakpoints"].as_array().unwrap();
    assert_eq!(breakpoints.len(), 3, "{response}");

    assert_eq!(breakpoints[0]["verified"], false, "{response}");
    assert_eq!(breakpoints[0]["line"], 6, "{response}");
    assert_breakpoint_contract(
        &breakpoints[0],
        "dap.breakpoint.conditionInvalid",
        "invalid-params",
        None,
    );

    assert_eq!(breakpoints[1]["verified"], false, "{response}");
    assert_eq!(breakpoints[1]["line"], 6, "{response}");
    assert_breakpoint_contract(
        &breakpoints[1],
        "dap.breakpoint.hitConditionInvalid",
        "invalid-params",
        None,
    );

    assert_eq!(breakpoints[2]["verified"], false, "{response}");
    assert_eq!(breakpoints[2]["line"], 6, "{response}");
    assert_breakpoint_contract(
        &breakpoints[2],
        "dap.breakpoint.logMessageInvalid",
        "invalid-params",
        None,
    );

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    assert_terminates_without_stopping(&mut client);
}

#[test]
fn dap_local_values_consume_runtime_debug_facts() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_value_contract_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request(
        "initialize",
        json!({ "supportsVariableType": true, "supportsVariablePaging": true }),
    );
    assert_eq!(client.response_for(init)["success"], true);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": true,
        }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 10 }],
        }),
    );
    assert_eq!(client.response_for(set)["success"], true);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");
    step_to_line(&mut client, 10);

    let threads = client.request("threads", json!({}));
    let thread_id = client.response_for(threads)["body"]["threads"][0]["id"]
        .as_i64()
        .unwrap();
    let stack = client.request("stackTrace", json!({ "threadId": thread_id }));
    let frames = client.response_for(stack);
    let frame_id = frames["body"]["stackFrames"][0]["id"].as_i64().unwrap();

    let scopes = client.request("scopes", json!({ "frameId": frame_id }));
    let scopes = client.response_for(scopes);
    let scope_list = scopes["body"]["scopes"].as_array().unwrap();
    let locals_ref = scope_list
        .iter()
        .find(|scope| scope["name"] == "Locals")
        .and_then(|scope| scope["variablesReference"].as_i64())
        .expect("a Locals scope");
    let durable_ref = scope_list
        .iter()
        .find(|scope| scope["name"] == "Durable Data")
        .and_then(|scope| scope["variablesReference"].as_i64())
        .expect("a Durable Data scope");

    let durable_variables =
        client.request("variables", json!({ "variablesReference": durable_ref }));
    let durable_variables = client.response_for(durable_variables);
    assert_eq!(
        durable_variables["body"]["variables"],
        json!([]),
        "a project with no saved roots has an empty Durable Data scope: {durable_variables}"
    );

    let locals = client.request("variables", json!({ "variablesReference": locals_ref }));
    let locals = client.response_for(locals);
    let local_vars = locals["body"]["variables"].as_array().unwrap();
    assert_eq!(local_vars.len(), 3, "{locals}");
    assert_eq!(locals["body"]["marrowDebug"]["visibleLocalCount"], 3);
    assert_eq!(locals["body"]["marrowDebug"]["localsTruncated"], false);
    let title = local_vars
        .iter()
        .find(|variable| variable["name"] == "title")
        .expect("a `title` local");
    assert_eq!(title["value"], "Dune", "{locals}");
    assert_runtime_debug_value(title);

    let local_page = client.request(
        "variables",
        json!({ "variablesReference": locals_ref, "start": 1, "count": 1 }),
    );
    let local_page = client.response_for(local_page);
    let local_page_vars = local_page["body"]["variables"].as_array().unwrap();
    assert_eq!(local_page_vars.len(), 1, "{local_page}");
    assert_eq!(local_page_vars[0]["name"], "title", "{local_page}");
    assert_eq!(local_page_vars[0]["value"], "Dune", "{local_page}");
    assert_eq!(local_page["body"]["marrowDebug"]["visibleLocalCount"], 3);
    assert_eq!(local_page["body"]["marrowDebug"]["localsTruncated"], true);

    let local_zero_count = client.request(
        "variables",
        json!({ "variablesReference": locals_ref, "start": 1, "count": 0 }),
    );
    let local_zero_count = client.response_for(local_zero_count);
    let local_zero_count_vars = local_zero_count["body"]["variables"].as_array().unwrap();
    assert_eq!(local_zero_count_vars.len(), 2, "{local_zero_count}");
    assert_eq!(
        local_zero_count_vars[0]["name"], "title",
        "{local_zero_count}"
    );
    assert_eq!(
        local_zero_count_vars[1]["name"], "tags",
        "{local_zero_count}"
    );

    let local_indexed = client.request(
        "variables",
        json!({ "variablesReference": locals_ref, "filter": "indexed" }),
    );
    let local_indexed = client.response_for(local_indexed);
    assert!(
        local_indexed["body"]["variables"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{local_indexed}"
    );

    let tags = local_vars
        .iter()
        .find(|variable| variable["name"] == "tags")
        .expect("a `tags` local");
    assert_eq!(tags["value"], "sequence[3]", "{locals}");
    assert_runtime_debug_value(tags);
    assert_eq!(tags["indexedVariables"], 3, "{locals}");
    assert_eq!(tags["marrowDebug"]["childrenTruncated"], false);
    let tags_ref = tags["variablesReference"].as_i64().unwrap();
    assert!(tags_ref > 0, "tags should be expandable: {locals}");

    let expanded = client.request("variables", json!({ "variablesReference": tags_ref }));
    let expanded = client.response_for(expanded);
    let expanded_vars = expanded["body"]["variables"].as_array().unwrap();
    assert_eq!(expanded_vars.len(), 3, "{expanded}");
    assert_eq!(expanded["body"]["marrowDebug"]["childrenTruncated"], false);
    let tag_child = &expanded["body"]["variables"][0];
    assert_eq!(tag_child["name"], "[1]", "{expanded}");
    assert_eq!(tag_child["value"], "classic", "{expanded}");
    assert_runtime_debug_value(tag_child);
    assert!(tag_child.get("marrowDebug").is_none(), "{expanded}");

    let zero_count = client.request(
        "variables",
        json!({ "variablesReference": tags_ref, "start": 1, "count": 0 }),
    );
    let zero_count = client.response_for(zero_count);
    let zero_count_vars = zero_count["body"]["variables"].as_array().unwrap();
    assert_eq!(zero_count_vars.len(), 2, "{zero_count}");
    assert_eq!(zero_count_vars[0]["name"], "[2]", "{zero_count}");
    assert_eq!(zero_count_vars[1]["name"], "[3]", "{zero_count}");

    let named_tags = client.request(
        "variables",
        json!({ "variablesReference": tags_ref, "filter": "named" }),
    );
    let named_tags = client.response_for(named_tags);
    assert!(
        named_tags["body"]["variables"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{named_tags}"
    );

    let indexed_tags = client.request(
        "variables",
        json!({ "variablesReference": tags_ref, "filter": "indexed", "start": 2, "count": 1 }),
    );
    let indexed_tags = client.response_for(indexed_tags);
    let indexed_tag_vars = indexed_tags["body"]["variables"].as_array().unwrap();
    assert_eq!(indexed_tag_vars.len(), 1, "{indexed_tags}");
    assert_eq!(indexed_tag_vars[0]["name"], "[3]", "{indexed_tags}");

    let page = client.request(
        "variables",
        json!({ "variablesReference": tags_ref, "start": 1, "count": 1 }),
    );
    let page = client.response_for(page);
    let page_vars = page["body"]["variables"].as_array().unwrap();
    assert_eq!(page_vars.len(), 1, "{page}");
    assert_eq!(page_vars[0]["name"], "[2]", "{page}");
    assert_eq!(page_vars[0]["value"], "space", "{page}");

    let durable_variables = client.request("variables", json!({ "variablesReference": 2 }));
    let durable_variables = client.response_for(durable_variables);
    assert_eq!(
        durable_variables["body"]["variables"],
        json!([]),
        "no saved roots are visible in this fixture: {durable_variables}"
    );

    let watch = client.request(
        "evaluate",
        json!({ "expression": "^books(1).title", "context": "watch" }),
    );
    let watch = client.response_for(watch);
    assert_evaluate_envelope_error(&watch, "dap.evaluate.expressionInvalid", "invalid-params");
}

#[test]
fn dap_large_local_sequences_preserve_truncation_facts() {
    let dir = tempfile::tempdir().unwrap();
    let (file, breakpoint_line) = write_large_sequence_fixture(dir.path(), 1001);

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({ "supportsVariablePaging": true }));
    assert_eq!(client.response_for(init)["success"], true);

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
        }),
    );
    assert_eq!(client.response_for(launch)["success"], true);
    client.event("initialized");

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": breakpoint_line }],
        }),
    );
    let response = client.response_for(set);
    assert_verified_breakpoint(
        &response["body"]["breakpoints"][0],
        i64::from(breakpoint_line),
    );

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");

    let threads = client.request("threads", json!({}));
    let thread_id = client.response_for(threads)["body"]["threads"][0]["id"]
        .as_i64()
        .unwrap();
    let stack = client.request("stackTrace", json!({ "threadId": thread_id }));
    let frames = client.response_for(stack);
    let frame_id = frames["body"]["stackFrames"][0]["id"].as_i64().unwrap();

    let scopes = client.request("scopes", json!({ "frameId": frame_id }));
    let scopes = client.response_for(scopes);
    let locals_ref = scopes["body"]["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scope| scope["name"] == "Locals")
        .and_then(|scope| scope["variablesReference"].as_i64())
        .expect("a Locals scope");

    let locals = client.request("variables", json!({ "variablesReference": locals_ref }));
    let locals = client.response_for(locals);
    assert_eq!(locals["body"]["marrowDebug"]["visibleLocalCount"], 1);
    assert_eq!(locals["body"]["marrowDebug"]["localsTruncated"], false);
    let local_vars = locals["body"]["variables"].as_array().unwrap();
    let items = local_vars
        .iter()
        .find(|variable| variable["name"] == "items")
        .expect("an `items` local");
    assert_eq!(items["value"], "sequence[1001]", "{locals}");
    assert_eq!(items["indexedVariables"], 1000, "{locals}");
    assert_eq!(items["marrowDebug"]["childrenTruncated"], true);
    assert!(items.get("marrowContract").is_none(), "{locals}");
    let items_ref = items["variablesReference"].as_i64().unwrap();
    assert!(items_ref > 0, "items should be expandable: {locals}");

    let last_captured = client.request(
        "variables",
        json!({
            "variablesReference": items_ref,
            "filter": "indexed",
            "start": 999,
            "count": 1,
        }),
    );
    let last_captured = client.response_for(last_captured);
    assert_eq!(
        last_captured["body"]["marrowDebug"]["childrenTruncated"],
        true
    );
    let last_captured_vars = last_captured["body"]["variables"].as_array().unwrap();
    assert_eq!(last_captured_vars.len(), 1, "{last_captured}");
    assert_eq!(last_captured_vars[0]["name"], "[1000]", "{last_captured}");
    assert_eq!(last_captured_vars[0]["value"], "1000", "{last_captured}");

    let omitted = client.request(
        "variables",
        json!({
            "variablesReference": items_ref,
            "filter": "indexed",
            "start": 1000,
            "count": 1,
        }),
    );
    let omitted = client.response_for(omitted);
    assert_eq!(omitted["body"]["variables"], json!([]), "{omitted}");
    assert_eq!(omitted["body"]["marrowDebug"]["childrenTruncated"], true);
}

#[test]
fn durable_data_scope_is_empty_without_saved_roots() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({ "project": dir.path().display().to_string(), "stopOnEntry": true }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    assert_eq!(client.response_for(set)["success"], true);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");
    step_to_line(&mut client, 6);

    let threads = client.request("threads", json!({}));
    let thread_id = client.response_for(threads)["body"]["threads"][0]["id"]
        .as_i64()
        .unwrap();
    let stack = client.request("stackTrace", json!({ "threadId": thread_id }));
    let frames = client.response_for(stack);
    let frame_id = frames["body"]["stackFrames"][0]["id"].as_i64().unwrap();

    let scopes = client.request("scopes", json!({ "frameId": frame_id }));
    let scopes = client.response_for(scopes);
    let scope_list = scopes["body"]["scopes"].as_array().unwrap();
    let durable_ref = scope_list
        .iter()
        .find(|scope| scope["name"] == "Durable Data")
        .and_then(|scope| scope["variablesReference"].as_i64())
        .expect("a Durable Data scope");

    let manual_durable_variables =
        client.request("variables", json!({ "variablesReference": durable_ref }));
    let manual_durable_variables = client.response_for(manual_durable_variables);
    assert_eq!(
        manual_durable_variables["body"]["variables"],
        json!([]),
        "no saved roots are visible in this fixture: {manual_durable_variables}"
    );

    let local_watch = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "watch" }),
    );
    let local_watch = client.response_for(local_watch);
    assert_evaluate_runtime_debug_value(&local_watch, "Dune", 0);
    assert!(
        local_watch["body"]
            .get("marrowDebug")
            .and_then(|debug| debug.get("debugDataBoundary"))
            .is_none(),
        "local-only watch evaluation should not claim a data boundary: {local_watch}"
    );
}

#[test]
fn durable_watch_evaluates_against_frame_data() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_durable_debug_data_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({ "project": dir.path().display().to_string(), "stopOnEntry": true }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 9 }],
        }),
    );
    assert_eq!(client.response_for(set)["success"], true);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");
    step_to_line(&mut client, 9);

    let durable_watch = client.request(
        "evaluate",
        json!({ "expression": "(^books(id).title ?? \"\")", "context": "watch" }),
    );
    let durable_watch = client.response_for(durable_watch);
    assert_evaluate_runtime_debug_value(&durable_watch, "Dune", 0);
    let store_snapshot =
        &durable_watch["body"]["marrowDebug"]["debugDataBoundary"]["storeSnapshot"];
    assert_runtime_store_snapshot(store_snapshot);
    assert_debug_data_boundary(
        &durable_watch["body"]["marrowDebug"]["debugDataBoundary"],
        store_snapshot,
    );

    let root_durable_watch = client.request(
        "evaluate",
        json!({ "expression": "^books(id)", "context": "watch" }),
    );
    let root_durable_watch = client.response_for(root_durable_watch);
    assert_eq!(root_durable_watch["success"], true, "{root_durable_watch}");
    let root_ref = root_durable_watch["body"]["variablesReference"]
        .as_i64()
        .expect("saved record watch expands");

    let root_children = client.request("variables", json!({ "variablesReference": root_ref }));
    let root_children = client.response_for(root_children);
    let store_snapshot =
        &root_children["body"]["marrowDebug"]["debugDataBoundary"]["storeSnapshot"];
    assert_runtime_store_snapshot(store_snapshot);
    assert_debug_data_boundary(
        &root_children["body"]["marrowDebug"]["debugDataBoundary"],
        store_snapshot,
    );
}

#[test]
fn durable_watch_inside_transaction_carries_open_transaction_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_durable_debug_transaction_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({ "project": dir.path().display().to_string(), "stopOnEntry": true }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 10 }],
        }),
    );
    assert_eq!(client.response_for(set)["success"], true);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");
    step_to_line(&mut client, 10);

    let durable_watch = client.request(
        "evaluate",
        json!({ "expression": "(^books(1).title ?? \"\")", "context": "watch" }),
    );
    let durable_watch = client.response_for(durable_watch);
    assert_evaluate_runtime_debug_value(&durable_watch, "Dune", 0);

    let boundary = &durable_watch["body"]["marrowDebug"]["debugDataBoundary"];
    let store_snapshot = &boundary["storeSnapshot"];
    assert_runtime_open_transaction_snapshot(store_snapshot, 1);
    assert_debug_data_boundary(boundary, store_snapshot);
}

#[test]
fn durable_data_scope_expands_frame_data_paths() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_durable_debug_data_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({ "project": dir.path().display().to_string(), "stopOnEntry": true }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 9 }],
        }),
    );
    assert_eq!(client.response_for(set)["success"], true);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");
    step_to_line(&mut client, 9);

    let stack = client.request("stackTrace", json!({ "threadId": 1 }));
    let frames = client.response_for(stack);
    let frame_id = frames["body"]["stackFrames"][0]["id"].as_i64().unwrap();
    let scopes = client.request("scopes", json!({ "frameId": frame_id }));
    let scopes = client.response_for(scopes);
    let durable_ref = scopes["body"]["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scope| scope["name"] == "Durable Data")
        .and_then(|scope| scope["variablesReference"].as_i64())
        .expect("a Durable Data scope");

    let roots = client.request("variables", json!({ "variablesReference": durable_ref }));
    let roots = client.response_for(roots);
    let store_snapshot = &roots["body"]["marrowDebug"]["storeSnapshot"];
    assert_runtime_store_snapshot(store_snapshot);
    assert_debug_data_boundary(
        &roots["body"]["marrowDebug"]["debugDataBoundary"],
        store_snapshot,
    );
    let books = roots["body"]["variables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variable| variable["name"] == "^books")
        .cloned()
        .expect("books root");
    let books_ref = books["variablesReference"].as_i64().expect("books expands");

    let records = client.request("variables", json!({ "variablesReference": books_ref }));
    let records = client.response_for(records);
    let book = records["body"]["variables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variable| variable["name"] == "^books(1)")
        .cloned()
        .expect("book record");
    let book_ref = book["variablesReference"].as_i64().expect("record expands");

    let fields = client.request("variables", json!({ "variablesReference": book_ref }));
    let fields = client.response_for(fields);
    let title = fields["body"]["variables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variable| variable["name"] == "^books(1).title")
        .expect("title field");
    assert_eq!(title["value"], "\"Dune\"", "{fields}");
    assert_eq!(title["variablesReference"], 0, "{fields}");
}

#[test]
fn durable_data_scope_honors_offset_pages_over_frame_data() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_durable_debug_data_paging_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({ "supportsVariablePaging": true }));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({ "project": dir.path().display().to_string(), "stopOnEntry": true }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 10 }],
        }),
    );
    assert_eq!(client.response_for(set)["success"], true);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");
    step_to_line(&mut client, 10);

    let stack = client.request("stackTrace", json!({ "threadId": 1 }));
    let frames = client.response_for(stack);
    let frame_id = frames["body"]["stackFrames"][0]["id"].as_i64().unwrap();
    let scopes = client.request("scopes", json!({ "frameId": frame_id }));
    let scopes = client.response_for(scopes);
    let durable_ref = scopes["body"]["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scope| scope["name"] == "Durable Data")
        .and_then(|scope| scope["variablesReference"].as_i64())
        .expect("a Durable Data scope");

    let roots = client.request("variables", json!({ "variablesReference": durable_ref }));
    let roots = client.response_for(roots);
    let books_ref = roots["body"]["variables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variable| variable["name"] == "^books")
        .and_then(|variable| variable["variablesReference"].as_i64())
        .expect("books root expands");

    let records = client.request(
        "variables",
        json!({ "variablesReference": books_ref, "start": 1, "count": 1 }),
    );
    let records = client.response_for(records);
    let variables = records["body"]["variables"].as_array().unwrap();
    assert_eq!(variables.len(), 1, "{records}");
    assert_eq!(variables[0]["name"], "^books(2)", "{records}");
    assert_eq!(variables[0]["value"], "", "{records}");
    assert!(
        variables[0]["variablesReference"]
            .as_i64()
            .is_some_and(|reference| reference > 0),
        "{records}"
    );
    assert_eq!(
        variables[0]["presentationHint"],
        json!({ "attributes": ["readOnly"], "kind": "data" }),
        "{records}"
    );
}

#[test]
fn native_durable_launch_evaluates_local_watch_against_the_admitted_session() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_durable_helper_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({ "project": dir.path().display().to_string(), "stopOnEntry": true }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 11 }],
        }),
    );
    assert_eq!(client.response_for(set)["success"], true);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");
    step_to_line(&mut client, 11);

    let local_watch = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "watch" }),
    );
    let local_watch = client.response_for(local_watch);
    assert_evaluate_runtime_debug_value(&local_watch, "Dune", 0);
}

#[test]
fn local_state_failures_expose_typed_contracts() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let cont = client.request("continue", json!({ "threadId": 1 }));
    let cont = client.response_for(cont);
    assert_response_marrow_error(&cont, "dap.notRunning", "invalid-state", None);

    let step = client.request("stepIn", json!({ "threadId": 1 }));
    let step = client.response_for(step);
    assert_response_marrow_error(&step, "dap.notStopped", "invalid-state", None);
}

#[test]
fn initialize_advertises_stopped_frame_hover_evaluate() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    let response = client.response_for(init);
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(
        response["body"]["supportsEvaluateForHovers"], true,
        "hover evaluation is backed by the same stopped-frame Marrow debug-expression facts as watch evaluate: {response}"
    );
    assert_eq!(
        response["body"]["supportsConditionalBreakpoints"], true,
        "conditional breakpoints are now backed by checked Marrow debug expressions: {response}"
    );
    assert_eq!(
        response["body"]["supportsHitConditionalBreakpoints"], true,
        "hit conditions are DAP hit-count bookkeeping over Marrow stop-point facts: {response}"
    );
    assert_eq!(
        response["body"]["supportsLogPoints"], true,
        "logpoint template expressions are checked and evaluated through Marrow debug-expression facts: {response}"
    );
}

#[test]
fn hover_evaluate_uses_stopped_frame_debug_expression_facts() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({ "project": dir.path().display().to_string(), "stopOnEntry": true }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    assert_eq!(client.response_for(set)["success"], true);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");
    step_to_line(&mut client, 6);

    let hover = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "hover" }),
    );
    let hover = client.response_for(hover);
    assert_evaluate_runtime_debug_value(&hover, "Dune", 0);
}

#[test]
fn stepping_advances_one_statement_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({ "project": dir.path().display().to_string(), "stopOnEntry": true }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({ "source": { "path": file.display().to_string() }, "breakpoints": [] }),
    );
    client.response_for(set);

    let done = client.request("configurationDone", json!({}));
    client.response_for(done);

    // stopOnEntry stops before the first statement (the `const id` on line 4).
    let entry = client.event("stopped");
    assert_eq!(entry["body"]["reason"], "entry", "{entry}");
    let first_line = stop_line(&mut client);
    assert_eq!(first_line, 4, "first stop on the first statement");

    // stepIn (next statement, any depth) -> the next statement, line 5.
    let step = client.request("stepIn", json!({ "threadId": 1 }));
    client.response_for(step);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "step", "{stopped}");
    assert_eq!(stop_line(&mut client), 5, "stepped to the next statement");

    // Continue to the end.
    let cont = client.request("continue", json!({ "threadId": 1 }));
    client.response_for(cont);
    client.event("terminated");
}

#[test]
fn terminate_while_stopped_unwinds_the_run_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);

    let launch = client.request(
        "launch",
        json!({ "project": dir.path().display().to_string(), "stopOnEntry": true }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    let set = client.request(
        "setBreakpoints",
        json!({ "source": { "path": file.display().to_string() }, "breakpoints": [] }),
    );
    client.response_for(set);

    let done = client.request("configurationDone", json!({}));
    client.response_for(done);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");

    // Terminate while stopped: the run unwinds (no print output reaches us) and
    // the session ends with a terminated event.
    let terminate = client.request("terminate", json!({}));
    assert_eq!(client.response_for(terminate)["success"], true);
    client.event("terminated");

    // The server should now close its stdout (the loop exited on `done`).
    let mut trailing = String::new();
    let _ = client.stdout.read_to_string(&mut trailing);
    assert!(
        !trailing.contains("Dune"),
        "a terminated run must not print: {trailing}"
    );
}

/// Ask for the current stack trace and return the stopped line.
fn stop_line(client: &mut Client) -> i64 {
    let stack = client.request("stackTrace", json!({ "threadId": 1 }));
    let stack = client.response_for(stack);
    stack["body"]["stackFrames"][0]["line"].as_i64().unwrap()
}

fn step_to_line(client: &mut Client, target: i64) {
    for _ in 0..20 {
        if stop_line(client) == target {
            return;
        }
        let step = client.request("stepIn", json!({ "threadId": 1 }));
        assert_eq!(client.response_for(step)["success"], true);
        let stopped = client.event("stopped");
        assert_eq!(stopped["body"]["reason"], "step", "{stopped}");
    }
    panic!("did not reach line {target}");
}

fn assert_terminates_without_stopping(client: &mut Client) {
    for _ in 0..20 {
        let message = client.read();
        assert_ne!(
            message["event"], "stopped",
            "unsupported breakpoints must not arm runtime stops: {message}"
        );
        if message["type"] == "event" && message["event"] == "terminated" {
            return;
        }
    }
    panic!("run did not terminate within the bounded message loop");
}
