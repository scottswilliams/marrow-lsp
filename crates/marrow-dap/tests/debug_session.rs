//! End-to-end debug session over the real `marrow-dap` binary.
//!
//! Spawns the built binary, drives the DAP handshake over its stdio with
//! `Content-Length` framing, launches a fixture entry with stop-on-entry, and
//! asserts the run stops, exposes a live local, blocks durable inspection, then
//! continues to a clean termination. This exercises the whole rendezvous through
//! the wire protocol, not internal APIs.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use marrow_run::{Host, ProjectOpen, ProjectSession, SessionEntry};
use serde_json::{Value as Json, json};

/// A minimal DAP client over a child process's stdio.
struct Client {
    child: Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    seq: i64,
}

impl Client {
    /// Launch the built `marrow-dap` binary and wrap its stdio.
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_marrow-dap"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn marrow-dap");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Client {
            child,
            stdin,
            stdout,
            seq: 0,
        }
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
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        self.stdin.write_all(&body).unwrap();
        self.stdin.flush().unwrap();
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
    let source = "module shelf\n\
                  \n\
                  pub fn main()\n\
                  \x20   const id: int = 1\n\
                  \x20   const title: string = \"Dune\"\n\
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

fn initialized_client() -> Client {
    let mut client = Client::spawn();
    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);
    client.event("initialized");
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

/// Write a fixture whose entry would need a launch argument if that surface were
/// available. Launch blocks before running it until Marrow exposes typed facts.
fn write_blocked_arg_fixture(dir: &Path) {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let source = "module shelf\n\
                  \n\
                  pub fn main(title: string)\n\
                  \x20   print(title)\n";
    std::fs::write(src.join("shelf.mw"), source).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"memory\" } }",
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

fn remove_catalog_after_native_seed(dir: &Path) {
    let session = ProjectSession::open(dir, ProjectOpen::run()).expect("seed session");
    let host = Host::new();
    let mut output = String::new();
    session
        .invoke(SessionEntry::new("shelf::main", &host, &mut output))
        .expect("seed run");

    let catalog = dir.join(marrow_project::CATALOG_FILE_NAME);
    assert!(catalog.exists(), "seed run should write accepted catalog");
    assert!(
        dir.join("data").join("marrow.redb").exists(),
        "seed run should create the native store"
    );
    std::fs::remove_file(catalog).expect("remove accepted catalog");
}

fn corrupt_catalog_utf8_after_native_seed(dir: &Path) {
    let session = ProjectSession::open(dir, ProjectOpen::run()).expect("seed session");
    let host = Host::new();
    let mut output = String::new();
    session
        .invoke(SessionEntry::new("shelf::main", &host, &mut output))
        .expect("seed run");

    let catalog = dir.join(marrow_project::CATALOG_FILE_NAME);
    assert!(catalog.exists(), "seed run should write accepted catalog");
    std::fs::write(catalog, [0xff]).expect("write invalid UTF-8 catalog");
}

/// Write a fixture with an expandable local sequence so DAP value responses
/// exercise the local expansion contract while durable surfaces stay blocked.
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

fn assert_debug_admin_value_contract(value: &Json, missing_fact: &str) {
    assert!(
        value.get("type").is_none(),
        "DAP contract metadata should not masquerade as a value type: {value}"
    );

    let attributes = value["presentationHint"]["attributes"]
        .as_array()
        .unwrap_or_else(|| panic!("DAP value should include presentation attributes: {value}"));
    assert!(
        attributes.iter().any(|attribute| attribute == "readOnly"),
        "DAP value presentation should be read-only: {value}"
    );

    let contract = value["marrowContract"]
        .as_object()
        .unwrap_or_else(|| panic!("DAP value should include Marrow contract metadata: {value}"));
    assert_eq!(
        contract.get("status").and_then(Json::as_str),
        Some("blocked-on-marrow"),
        "DAP value contract should mark blocked status: {value}"
    );
    assert_eq!(
        contract.get("blockedOn").and_then(Json::as_str),
        Some(missing_fact),
        "DAP value contract should name `{missing_fact}`: {value}"
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

fn assert_blocked_response(response: &Json, code: &str, blocked_on: &str) {
    assert_response_marrow_error(response, code, "blocked-on-marrow", Some(blocked_on));
}

fn assert_breakpoint_marrow_contract(breakpoint: &Json, code: &str, blocked_on: &str) {
    assert_breakpoint_contract(breakpoint, code, "blocked-on-marrow", Some(blocked_on));
}

fn assert_breakpoint_contract(
    breakpoint: &Json,
    code: &str,
    status: &str,
    blocked_on: Option<&str>,
) {
    let contract = breakpoint["marrowContract"].as_object().unwrap_or_else(|| {
        panic!("advisory breakpoint should include marrowContract: {breakpoint}")
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

#[test]
fn malformed_request_arguments_fail_before_command_handlers() {
    for (command, arguments) in [
        ("initialize", json!([])),
        ("launch", json!([])),
        ("setBreakpoints", json!([])),
        ("configurationDone", json!("bad")),
        ("threads", json!(false)),
        ("stackTrace", json!(1)),
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
    client.event("initialized");

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
    let mut client = Client::spawn();

    let request = client.request("madeUpCommand", json!([]));
    let response = client.response_for(request);
    assert_response_marrow_error(
        &response,
        "dap.unsupportedRequest",
        "unsupported-request",
        None,
    );
}

#[test]
fn breakpoint_expression_inputs_return_distinct_blocked_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);
    client.event("initialized");

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
    assert_breakpoint_marrow_contract(
        &breakpoints[0],
        "dap.breakpoint.unverified",
        "canonical stop-point facts",
    );

    for (index, line) in [(1, 7), (2, 8), (3, 9)] {
        let breakpoint = &breakpoints[index];
        assert_eq!(breakpoint["verified"], false, "{response}");
        assert_eq!(breakpoint["line"], line, "{response}");
        assert_breakpoint_marrow_contract(
            breakpoint,
            "dap.breakpoint.expressionBlocked",
            "canonical stop-point and breakpoint expression facts",
        );
        assert!(
            breakpoint.get("condition").is_none()
                && breakpoint.get("hitCondition").is_none()
                && breakpoint.get("logMessage").is_none(),
            "blocked breakpoint response must not echo or retain expression inputs: {breakpoint}"
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
    client.event("initialized");

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
    assert_breakpoint_marrow_contract(
        &breakpoints[0],
        "dap.breakpoint.unverified",
        "canonical stop-point facts",
    );

    for breakpoint in &breakpoints[1..=7] {
        assert_invalid_breakpoint_line(breakpoint);
    }

    assert_eq!(breakpoints[8]["line"], 7, "{response}");
    assert_breakpoint_marrow_contract(
        &breakpoints[8],
        "dap.breakpoint.expressionBlocked",
        "canonical stop-point and breakpoint expression facts",
    );
}

#[test]
fn malformed_breakpoint_source_envelopes_fail_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);
    client.event("initialized");

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
fn source_reference_breakpoint_sources_are_adapter_unsupported() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);
    client.event("initialized");

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
        "dap.breakpoint.sourceReferenceUnsupported",
        "unsupported-request",
    );
}

#[test]
fn positive_source_reference_takes_precedence_over_path() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);
    client.event("initialized");

    let set = client.request(
        "setBreakpoints",
        json!({
            "source": {
                "path": "ignored.mw",
                "sourceReference": 1,
            },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let response = client.response_for(set);
    assert_breakpoint_envelope_error(
        &response,
        "dap.breakpoint.sourceReferenceUnsupported",
        "unsupported-request",
    );
}

#[test]
fn zero_source_reference_with_path_uses_the_path_source() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);
    client.event("initialized");

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
    assert_breakpoint_marrow_contract(
        &response["body"]["breakpoints"][0],
        "dap.breakpoint.unverified",
        "canonical stop-point facts",
    );
}

#[test]
fn non_array_breakpoints_fail_the_request() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);
    client.event("initialized");

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
    client.event("initialized");

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
    client.event("initialized");

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
    client.event("initialized");

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
fn valid_evaluate_envelopes_keep_blocked_contracts() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    assert_eq!(client.response_for(init)["success"], true);
    client.event("initialized");

    let unknown_context = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "clipboard" }),
    );
    let unknown_context = client.response_for(unknown_context);
    assert_blocked_response(
        &unknown_context,
        "dap.evaluate.blocked",
        "canonical expression/evaluate facts",
    );

    let whitespace = client.request(
        "evaluate",
        json!({ "expression": "   ", "context": "watch" }),
    );
    let whitespace = client.response_for(whitespace);
    assert_blocked_response(
        &whitespace,
        "dap.evaluate.blocked",
        "canonical expression/evaluate facts",
    );

    let hover = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "hover" }),
    );
    let hover = client.response_for(hover);
    assert_blocked_response(
        &hover,
        "dap.hoverEvaluate.blocked",
        "canonical evaluate facts",
    );

    let durable = client.request(
        "evaluate",
        json!({ "expression": "^books(1).title", "context": "watch" }),
    );
    let durable = client.response_for(durable);
    assert_blocked_response(
        &durable,
        "dap.durableData.blocked",
        "typed durable watch/path facts",
    );
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
fn launch_blocks_non_empty_args_by_default_until_typed_facts_exist() {
    let dir = tempfile::tempdir().unwrap();
    write_blocked_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "args": ["Dune"],
        }),
    );
    let blocked = client.response_for(launch);
    assert_blocked_response(
        &blocked,
        "dap.launchArgs.blocked",
        "typed launch argument decoding facts",
    );
}

#[test]
fn launch_rejects_non_array_args_as_protocol_validation() {
    let dir = tempfile::tempdir().unwrap();
    write_blocked_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

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
}

#[test]
fn launch_rejects_concrete_non_string_entry_as_protocol_validation() {
    let dir = tempfile::tempdir().unwrap();
    write_blocked_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

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
    write_blocked_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

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
    write_blocked_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

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
    write_blocked_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

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
    client.event("initialized");

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
    write_blocked_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

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
    client.event("initialized");

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
    client.event("initialized");

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
        }),
    );
    let response = client.response_for(launch);
    assert_launch_success(&response);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);

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
        !dir.path().join("marrow.catalog.json").exists(),
        "isolated DAP launch must not write the accepted catalog"
    );
    assert!(
        !dir.path().join("data").join("marrow.redb").exists(),
        "isolated DAP launch must not open or create the native store"
    );
}

#[test]
fn launch_blocks_native_store_catalog_repair_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    write_native_identity_fixture(dir.path());
    remove_catalog_after_native_seed(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
        }),
    );
    let blocked = client.response_for(launch);
    assert_blocked_response(
        &blocked,
        "dap.launchCatalogRepair.blocked",
        "read-only debug catalog admission facts",
    );
    assert!(
        !dir.path().join(marrow_project::CATALOG_FILE_NAME).exists(),
        "DAP launch must not repair the accepted catalog artifact"
    );
}

#[test]
fn launch_blocks_invalid_utf8_catalog_repair_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    write_native_identity_fixture(dir.path());
    corrupt_catalog_utf8_after_native_seed(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
        }),
    );
    let blocked = client.response_for(launch);
    assert_blocked_response(
        &blocked,
        "dap.launchCatalogRepair.blocked",
        "read-only debug catalog admission facts",
    );
    assert_eq!(
        std::fs::read(dir.path().join(marrow_project::CATALOG_FILE_NAME)).unwrap(),
        [0xff],
        "DAP launch must not repair the invalid catalog artifact"
    );
}

#[test]
fn launch_rejects_explicit_entry_strings_until_canonical_facts_exist() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "entry": "shelf::main",
        }),
    );
    let blocked = client.response_for(launch);
    assert_blocked_response(
        &blocked,
        "dap.launchEntry.blocked",
        "canonical function-entry facts",
    );
}

#[test]
fn advisory_breakpoint_does_not_arm_but_stepped_stop_exposes_locals() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = Client::spawn();

    // initialize -> response + initialized event.
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
    client.event("initialized");

    // launch the fixture's default entry.
    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "stopOnEntry": true,
        }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    // The print statement is line 6; the breakpoint is advisory only.
    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let advisory = client.response_for(set);
    let breakpoints = advisory["body"]["breakpoints"].as_array().unwrap();
    assert_eq!(breakpoints.len(), 1, "{advisory}");
    assert_eq!(breakpoints[0]["verified"], false, "{advisory}");
    assert_eq!(breakpoints[0]["line"], 6, "{advisory}");
    assert_breakpoint_marrow_contract(
        &breakpoints[0],
        "dap.breakpoint.unverified",
        "canonical stop-point facts",
    );

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
    assert_eq!(
        frame["source"]["path"],
        file.display().to_string(),
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
    assert!(
        scope_list.iter().all(|scope| !scope["name"]
            .as_str()
            .unwrap_or_default()
            .starts_with("^ Durable Data")),
        "durable scope must stay hidden: {scopes}"
    );

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
    assert_debug_admin_value_contract(title, "canonical runtime value expansion facts");

    // Locals remain available through the Locals scope, but watch/REPL
    // expression evaluation waits for canonical Marrow evaluate facts.
    for expression in ["title", "_x1", "1 + 1"] {
        let blocked = client.request(
            "evaluate",
            json!({ "expression": expression, "context": "watch" }),
        );
        let blocked = client.response_for(blocked);
        assert_blocked_response(
            &blocked,
            "dap.evaluate.blocked",
            "canonical expression/evaluate facts",
        );
    }

    let durable_variables = client.request("variables", json!({ "variablesReference": 2 }));
    let durable_variables = client.response_for(durable_variables);
    assert_blocked_response(
        &durable_variables,
        "dap.durableData.blocked",
        "typed durable watch/path facts",
    );

    let watch = client.request(
        "evaluate",
        json!({ "expression": "^books(1).title", "context": "watch" }),
    );
    let watch = client.response_for(watch);
    assert_blocked_response(
        &watch,
        "dap.durableData.blocked",
        "typed durable watch/path facts",
    );

    // A non-^ expression is refused at the session boundary.
    let bad = client.request(
        "evaluate",
        json!({ "expression": "1 + 1", "context": "watch" }),
    );
    let bad = client.response_for(bad);
    assert_blocked_response(
        &bad,
        "dap.evaluate.blocked",
        "canonical expression/evaluate facts",
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
fn advisory_breakpoint_does_not_stop_without_stop_on_entry() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

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
            "breakpoints": [{ "line": 6 }],
        }),
    );
    let advisory = client.response_for(set);
    assert_eq!(advisory["body"]["breakpoints"][0]["verified"], false);
    assert_breakpoint_marrow_contract(
        &advisory["body"]["breakpoints"][0],
        "dap.breakpoint.unverified",
        "canonical stop-point facts",
    );

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    assert_terminates_without_stopping(&mut client);
}

#[test]
fn dap_value_surfaces_mark_blocked_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_value_contract_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request(
        "initialize",
        json!({ "supportsVariableType": true, "supportsVariablePaging": true }),
    );
    assert_eq!(client.response_for(init)["success"], true);
    client.event("initialized");

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
    assert!(
        scope_list.iter().all(|scope| !scope["name"]
            .as_str()
            .unwrap_or_default()
            .starts_with("^ Durable Data")),
        "durable scope must stay hidden: {scopes}"
    );

    let locals = client.request("variables", json!({ "variablesReference": locals_ref }));
    let locals = client.response_for(locals);
    let local_vars = locals["body"]["variables"].as_array().unwrap();
    assert_eq!(local_vars.len(), 3, "{locals}");
    let title = local_vars
        .iter()
        .find(|variable| variable["name"] == "title")
        .expect("a `title` local");
    assert_eq!(title["value"], "Dune", "{locals}");
    assert_debug_admin_value_contract(title, "canonical runtime value expansion facts");

    let local_page = client.request(
        "variables",
        json!({ "variablesReference": locals_ref, "start": 1, "count": 1 }),
    );
    let local_page = client.response_for(local_page);
    let local_page_vars = local_page["body"]["variables"].as_array().unwrap();
    assert_eq!(local_page_vars.len(), 1, "{local_page}");
    assert_eq!(local_page_vars[0]["name"], "title", "{local_page}");
    assert_eq!(local_page_vars[0]["value"], "Dune", "{local_page}");

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
    assert_debug_admin_value_contract(tags, "canonical runtime value expansion facts");
    assert_eq!(tags["indexedVariables"], 3, "{locals}");
    let tags_ref = tags["variablesReference"].as_i64().unwrap();
    assert!(tags_ref > 0, "tags should be expandable: {locals}");

    let expanded = client.request("variables", json!({ "variablesReference": tags_ref }));
    let expanded = client.response_for(expanded);
    let expanded_vars = expanded["body"]["variables"].as_array().unwrap();
    assert_eq!(expanded_vars.len(), 3, "{expanded}");
    let tag_child = &expanded["body"]["variables"][0];
    assert_eq!(tag_child["name"], "[0]", "{expanded}");
    assert_eq!(tag_child["value"], "classic", "{expanded}");
    assert_debug_admin_value_contract(tag_child, "canonical runtime value expansion facts");

    let zero_count = client.request(
        "variables",
        json!({ "variablesReference": tags_ref, "start": 1, "count": 0 }),
    );
    let zero_count = client.response_for(zero_count);
    let zero_count_vars = zero_count["body"]["variables"].as_array().unwrap();
    assert_eq!(zero_count_vars.len(), 2, "{zero_count}");
    assert_eq!(zero_count_vars[0]["name"], "[1]", "{zero_count}");
    assert_eq!(zero_count_vars[1]["name"], "[2]", "{zero_count}");

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
    assert_eq!(indexed_tag_vars[0]["name"], "[2]", "{indexed_tags}");

    let page = client.request(
        "variables",
        json!({ "variablesReference": tags_ref, "start": 1, "count": 1 }),
    );
    let page = client.response_for(page);
    let page_vars = page["body"]["variables"].as_array().unwrap();
    assert_eq!(page_vars.len(), 1, "{page}");
    assert_eq!(page_vars[0]["name"], "[1]", "{page}");
    assert_eq!(page_vars[0]["value"], "space", "{page}");

    let durable_variables = client.request("variables", json!({ "variablesReference": 2 }));
    let durable_variables = client.response_for(durable_variables);
    assert_blocked_response(
        &durable_variables,
        "dap.durableData.blocked",
        "typed durable watch/path facts",
    );

    let watch = client.request(
        "evaluate",
        json!({ "expression": "^books(1).title", "context": "watch" }),
    );
    let watch = client.response_for(watch);
    assert_blocked_response(
        &watch,
        "dap.durableData.blocked",
        "typed durable watch/path facts",
    );
}

#[test]
fn durable_data_inspection_is_blocked_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

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
    assert!(
        scope_list.iter().all(|scope| !scope["name"]
            .as_str()
            .unwrap_or_default()
            .starts_with("^ Durable Data")),
        "durable-data scope must be hidden by default: {scopes}"
    );

    let manual_durable_variables = client.request("variables", json!({ "variablesReference": 2 }));
    let manual_durable_variables = client.response_for(manual_durable_variables);
    assert_blocked_response(
        &manual_durable_variables,
        "dap.durableData.blocked",
        "typed durable watch/path facts",
    );

    let local_watch = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "watch" }),
    );
    let local_watch = client.response_for(local_watch);
    assert_blocked_response(
        &local_watch,
        "dap.evaluate.blocked",
        "canonical expression/evaluate facts",
    );

    let durable_watch = client.request(
        "evaluate",
        json!({ "expression": "  ^books(1).title", "context": "watch" }),
    );
    let durable_watch = client.response_for(durable_watch);
    assert_blocked_response(
        &durable_watch,
        "dap.durableData.blocked",
        "typed durable watch/path facts",
    );
}

#[test]
fn local_state_failures_expose_typed_contracts() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

    let cont = client.request("continue", json!({ "threadId": 1 }));
    let cont = client.response_for(cont);
    assert_response_marrow_error(&cont, "dap.notRunning", "invalid-state", None);

    let step = client.request("stepIn", json!({ "threadId": 1 }));
    let step = client.response_for(step);
    assert_response_marrow_error(&step, "dap.notStopped", "invalid-state", None);
}

#[test]
fn hover_evaluate_is_blocked_until_canonical_facts_exist_initialize_capability() {
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    let response = client.response_for(init);
    assert_eq!(response["success"], true, "{response}");
    assert_ne!(
        response["body"]["supportsEvaluateForHovers"], true,
        "hover evaluation must not be advertised until Marrow exposes canonical evaluate facts: {response}"
    );
    for capability in [
        "supportsConditionalBreakpoints",
        "supportsHitConditionalBreakpoints",
        "supportsLogPoints",
    ] {
        assert_ne!(
            response["body"][capability], true,
            "{capability} must not be advertised until Marrow exposes canonical breakpoint facts: {response}"
        );
    }
}

#[test]
fn hover_evaluate_is_blocked_until_canonical_facts_exist_request() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

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
    assert_blocked_response(
        &hover,
        "dap.hoverEvaluate.blocked",
        "canonical evaluate facts",
    );
}

#[test]
fn stepping_advances_one_statement_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());
    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

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
    client.event("initialized");

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
            "advisory breakpoints must not arm runtime stops: {message}"
        );
        if message["type"] == "event" && message["event"] == "terminated" {
            return;
        }
    }
    panic!("run did not terminate within the bounded message loop");
}
