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
        self.seq += 1;
        let seq = self.seq;
        let message = json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        });
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
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" } }",
    )
    .unwrap();
    file
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

fn write_catalog_identity_fixture(dir: &Path) {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let source = "module shelf\n\
                  \n\
                  resource Book\n\
                  \x20   required title: string\n\
                  \n\
                  store ^books(id: int): Book\n\
                  \n\
                  pub fn main(): int\n\
                  \x20   return 1\n";
    std::fs::write(src.join("shelf.mw"), source).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" }, \"store\": { \"backend\": \"native\", \"dataDir\": \"data\" } }",
    )
    .unwrap();
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
                  \x20   print(title)\n";
    let file = src.join("shelf.mw");
    std::fs::write(&file, source).unwrap();
    std::fs::write(
        dir.join("marrow.json"),
        "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"shelf::main\" } }",
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
        Some("blocked-on-marrow"),
        "breakpoint should expose blocked status: {breakpoint}"
    );
    assert_eq!(
        contract.get("blockedOn").and_then(Json::as_str),
        Some(blocked_on),
        "breakpoint should name `{blocked_on}`: {breakpoint}"
    );
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
fn launch_blocks_catalog_identity_without_typed_admission_facts() {
    let dir = tempfile::tempdir().unwrap();
    write_catalog_identity_fixture(dir.path());
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
        "dap.launchCatalogIdentity.blocked",
        "accepted catalog identity facts",
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
    assert!(
        frame.get("source").is_none(),
        "stack source stays unavailable until Marrow exposes canonical debugger source facts: {frames}"
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

    let init = client.request("initialize", json!({ "supportsVariableType": true }));
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
            "breakpoints": [{ "line": 8 }],
        }),
    );
    assert_eq!(client.response_for(set)["success"], true);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "entry", "{stopped}");
    step_to_line(&mut client, 8);

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
    let title = local_vars
        .iter()
        .find(|variable| variable["name"] == "title")
        .expect("a `title` local");
    assert_eq!(title["value"], "Dune", "{locals}");
    assert_debug_admin_value_contract(title, "canonical runtime value expansion facts");

    let tags = local_vars
        .iter()
        .find(|variable| variable["name"] == "tags")
        .expect("a `tags` local");
    assert_eq!(tags["value"], "sequence[1]", "{locals}");
    assert_debug_admin_value_contract(tags, "canonical runtime value expansion facts");
    let tags_ref = tags["variablesReference"].as_i64().unwrap();
    assert!(tags_ref > 0, "tags should be expandable: {locals}");

    let expanded = client.request("variables", json!({ "variablesReference": tags_ref }));
    let expanded = client.response_for(expanded);
    let tag_child = &expanded["body"]["variables"][0];
    assert_eq!(tag_child["name"], "[0]", "{expanded}");
    assert_eq!(tag_child["value"], "classic", "{expanded}");
    assert_debug_admin_value_contract(tag_child, "canonical runtime value expansion facts");

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
