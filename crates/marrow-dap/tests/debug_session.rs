//! End-to-end debug session over the real `marrow-dap` binary.
//!
//! Spawns the built binary, drives the DAP handshake over its stdio with
//! `Content-Length` framing, launches a fixture entry with a breakpoint, and
//! asserts the run stops, exposes a live local, and can opt into a raw
//! `^ Durable Data` value at the stop, then continues to a clean termination.
//! This exercises the whole
//! rendezvous — run-thread stepping, parked-frame queries, the durable scope —
//! through the wire protocol, not internal APIs.

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

/// Write a self-contained fixture project with a memory store and an entry that
/// writes a `^` value and binds a local the debugger can read at a breakpoint.
fn write_fixture(dir: &Path) -> std::path::PathBuf {
    let src = dir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let source = "module shelf\n\
                  \n\
                  resource Book at ^books(id: int)\n\
                  \x20   required title: string\n\
                  \n\
                  pub fn main()\n\
                  \x20   const id: int = 1\n\
                  \x20   const title: string = \"Dune\"\n\
                  \x20   ^books(id).title = title\n\
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

/// Write a fixture whose entry consumes one scalar launch argument, so the
/// prototype decoder path has to thread the value into the real run.
fn write_arg_fixture(dir: &Path) {
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

#[test]
fn launch_blocks_non_empty_args_by_default_until_typed_facts_exist() {
    let dir = tempfile::tempdir().unwrap();
    write_arg_fixture(dir.path());

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
    assert_eq!(blocked["success"], false, "{blocked}");
    let message = blocked["message"].as_str().unwrap();
    assert!(
        message.contains("blocked-on-marrow"),
        "non-empty args rejection should name the Marrow blocker: {blocked}"
    );
    assert!(
        message.contains("typed launch argument decoding facts"),
        "non-empty args rejection should name the missing facts: {blocked}"
    );
}

#[test]
fn launch_rejects_non_array_args_as_protocol_validation() {
    let dir = tempfile::tempdir().unwrap();
    write_arg_fixture(dir.path());

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
    assert_eq!(rejected["success"], false, "{rejected}");
    let message = rejected["message"].as_str().unwrap();
    assert!(
        message.contains("`args` must be an array"),
        "non-array args should fail protocol validation: {rejected}"
    );
    assert!(
        !message.contains("blocked-on-marrow"),
        "non-array args should not be reported as a semantic blocker: {rejected}"
    );
}

#[test]
fn launch_allows_prototype_args_only_with_explicit_opt_in() {
    let dir = tempfile::tempdir().unwrap();
    write_arg_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

    let launch = client.request(
        "launch",
        json!({
            "project": dir.path().display().to_string(),
            "args": ["Dune"],
            "allowPrototypeArgs": true,
        }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);

    let output = client.event("output");
    assert!(
        output["body"]["output"].as_str().unwrap().contains("Dune"),
        "{output}"
    );
    client.event("terminated");
}

#[test]
fn a_breakpoint_exposes_a_local_and_a_durable_value_then_continues() {
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
            "stopOnEntry": false,
            "allowRawDataInspection": true,
        }),
    );
    assert_eq!(client.response_for(launch)["success"], true);

    // Break on the print statement (line 10) — after the ^ write and with both
    // locals (id, title) in scope.
    let set = client.request(
        "setBreakpoints",
        json!({
            "source": { "path": file.display().to_string() },
            "breakpoints": [{ "line": 10 }],
        }),
    );
    let verified = client.response_for(set);
    let breakpoints = verified["body"]["breakpoints"].as_array().unwrap();
    assert_eq!(breakpoints.len(), 1, "{verified}");
    assert_eq!(breakpoints[0]["verified"], true, "{verified}");
    assert_eq!(breakpoints[0]["line"], 10, "{verified}");

    // configurationDone starts the run; it should hit the breakpoint and stop.
    let done = client.request("configurationDone", json!({}));
    assert_eq!(client.response_for(done)["success"], true);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");

    // threads + stackTrace + scopes.
    let threads = client.request("threads", json!({}));
    let thread_id = client.response_for(threads)["body"]["threads"][0]["id"]
        .as_i64()
        .unwrap();
    let stack = client.request("stackTrace", json!({ "threadId": thread_id }));
    let frames = client.response_for(stack);
    let frame = &frames["body"]["stackFrames"][0];
    assert_eq!(frame["line"], 10, "{frames}");
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
        .find(|scope| scope["name"] == "^ Durable Data (debug/admin prototype)")
        .and_then(|scope| scope["variablesReference"].as_i64())
        .expect("a durable scope");

    // The Locals scope exposes `title = "Dune"` (a string local in scope).
    let locals = client.request("variables", json!({ "variablesReference": locals_ref }));
    let locals = client.response_for(locals);
    let local_vars = locals["body"]["variables"].as_array().unwrap();
    let title = local_vars
        .iter()
        .find(|variable| variable["name"] == "title")
        .expect("a `title` local");
    assert_eq!(title["value"], "Dune", "{locals}");

    // Locals remain available through the Locals scope, but watch/REPL
    // expression evaluation waits for canonical Marrow evaluate facts.
    for expression in ["title", "_x1", "1 + 1"] {
        let blocked = client.request(
            "evaluate",
            json!({ "expression": expression, "context": "watch" }),
        );
        let blocked = client.response_for(blocked);
        assert_eq!(blocked["success"], false, "{blocked}");
        let message = blocked["message"].as_str().unwrap();
        assert!(
            message.contains("blocked-on-marrow"),
            "non-^ evaluate rejection should name the Marrow blocker: {blocked}"
        );
        assert!(
            message.contains("canonical expression/evaluate facts"),
            "non-^ evaluate rejection should name the missing facts: {blocked}"
        );
    }

    // The durable scope shows the root the run already wrote into this run's
    // store (read-your-writes): ^books exists with the value we wrote.
    let roots = client.request("variables", json!({ "variablesReference": durable_ref }));
    let roots = client.response_for(roots);
    let root_vars = roots["body"]["variables"].as_array().unwrap();
    let books = root_vars
        .iter()
        .find(|variable| variable["name"] == "^books")
        .expect("the ^books root");
    let books_ref = books["variablesReference"].as_i64().unwrap();
    assert!(
        books_ref > 0,
        "the ^books root should be expandable: {roots}"
    );

    // Drill ^books -> record (1) -> title = "Dune".
    let records = client.request("variables", json!({ "variablesReference": books_ref }));
    let records = client.response_for(records);
    let record = records["body"]["variables"][0].clone();
    let record_ref = record["variablesReference"].as_i64().unwrap();
    let fields = client.request("variables", json!({ "variablesReference": record_ref }));
    let fields = client.response_for(fields);
    let field_vars = fields["body"]["variables"].as_array().unwrap();
    let durable_title = field_vars
        .iter()
        .find(|variable| variable["name"] == "title")
        .expect("a durable title field");
    assert_eq!(durable_title["value"], "\"Dune\"", "{fields}");

    // A watch on a ^ path resolves through the live store.
    let watch = client.request(
        "evaluate",
        json!({ "expression": "^books(1).title", "context": "watch" }),
    );
    let watch = client.response_for(watch);
    assert_eq!(watch["success"], true, "{watch}");
    assert_eq!(watch["body"]["result"], "\"Dune\"", "{watch}");

    // A non-^ expression is refused at the session boundary, even when raw
    // durable-data inspection is enabled.
    let bad = client.request(
        "evaluate",
        json!({ "expression": "1 + 1", "context": "watch" }),
    );
    let bad = client.response_for(bad);
    assert_eq!(bad["success"], false, "{bad}");
    assert!(
        bad["message"]
            .as_str()
            .unwrap()
            .contains("canonical expression/evaluate facts"),
        "{bad}"
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
fn raw_durable_data_inspection_is_blocked_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_fixture(dir.path());

    let mut client = Client::spawn();

    let init = client.request("initialize", json!({}));
    client.response_for(init);
    client.event("initialized");

    let launch = client.request(
        "launch",
        json!({ "project": dir.path().display().to_string(), "stopOnEntry": false }),
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
    let scope_list = scopes["body"]["scopes"].as_array().unwrap();
    assert!(
        scope_list.iter().all(|scope| !scope["name"]
            .as_str()
            .unwrap_or_default()
            .starts_with("^ Durable Data")),
        "raw durable-data scope must be hidden by default: {scopes}"
    );

    let manual_durable_variables = client.request("variables", json!({ "variablesReference": 2 }));
    let manual_durable_variables = client.response_for(manual_durable_variables);
    assert_eq!(
        manual_durable_variables["success"], false,
        "raw durable-data variables must reject the fixed reference by default: {manual_durable_variables}"
    );
    assert!(
        manual_durable_variables["message"]
            .as_str()
            .unwrap()
            .contains("blocked-on-marrow"),
        "raw durable-data variables rejection should name the Marrow blocker: {manual_durable_variables}"
    );

    let local_watch = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "watch" }),
    );
    let local_watch = client.response_for(local_watch);
    assert_eq!(local_watch["success"], false, "{local_watch}");
    assert!(
        local_watch["message"]
            .as_str()
            .unwrap()
            .contains("canonical expression/evaluate facts"),
        "{local_watch}"
    );

    let durable_watch = client.request(
        "evaluate",
        json!({ "expression": "  ^books(1).title", "context": "watch" }),
    );
    let durable_watch = client.response_for(durable_watch);
    assert_eq!(durable_watch["success"], false, "{durable_watch}");
    let message = durable_watch["message"].as_str().unwrap();
    assert!(
        message.contains("blocked-on-marrow"),
        "raw durable-data rejection should name the Marrow blocker: {durable_watch}"
    );
    assert!(
        message.contains("typed durable watch/path facts")
            || message.contains("raw durable-data inspection"),
        "raw durable-data rejection should name the missing facts: {durable_watch}"
    );
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
        json!({ "project": dir.path().display().to_string(), "stopOnEntry": false }),
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
    assert_eq!(stopped["body"]["reason"], "breakpoint", "{stopped}");

    let hover = client.request(
        "evaluate",
        json!({ "expression": "title", "context": "hover" }),
    );
    let hover = client.response_for(hover);
    assert_eq!(hover["success"], false, "{hover}");
    let message = hover["message"].as_str().unwrap();
    assert!(
        message.contains("blocked-on-marrow"),
        "hover evaluate rejection should name the Marrow blocker: {hover}"
    );
    assert!(
        message.contains("canonical evaluate facts"),
        "hover evaluate rejection should name the missing facts: {hover}"
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

    // stopOnEntry stops before the first statement (the `const id` on line 7).
    let entry = client.event("stopped");
    assert_eq!(entry["body"]["reason"], "entry", "{entry}");
    let first_line = stop_line(&mut client);
    assert_eq!(first_line, 7, "first stop on the first statement");

    // stepIn (next statement, any depth) -> the next statement, line 8.
    let step = client.request("stepIn", json!({ "threadId": 1 }));
    client.response_for(step);
    let stopped = client.event("stopped");
    assert_eq!(stopped["body"]["reason"], "step", "{stopped}");
    assert_eq!(stop_line(&mut client), 8, "stepped to the next statement");

    // Continue to the end.
    let cont = client.request("continue", json!({ "threadId": 1 }));
    client.response_for(cont);
    client.event("terminated");
}

#[test]
fn terminate_at_a_breakpoint_unwinds_the_run_cleanly() {
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
