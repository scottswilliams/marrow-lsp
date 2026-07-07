//! The Marrow MCP server: a stdio Model Context Protocol transport for agents.
//!
//! All language intelligence lives in `marrow-lsp-core`; this binary is the thin
//! stdio transport over it — the second transport beside the LSP, sharing one
//! brain. It speaks newline-delimited JSON-RPC 2.0 on stdin/stdout (the MCP stdio
//! framing: one message per line, no embedded newlines, no `Content-Length`),
//! handles `initialize` / `tools/list` / `tools/call`, and routes each tool call
//! to [`marrow_lsp_core::mcp`]. Logging, if any, goes to stderr so stdout carries
//! only protocol messages.
//!
//! ## Data-access gate
//! Saved-root, data-inspection, and surface operation tools read a project's real
//! stored data, so they are a data-exfiltration boundary. They are disabled
//! unless the operator opts in at launch with `MARROW_MCP_ALLOW_DATA=1` (or the
//! `--allow-data` flag). When disabled, those tools return a clear refusal and
//! read nothing. Surface route listing reads only source/catalog facts and stays
//! outside this gate. Execution (`mw_run`) is always sandboxed (a fresh in-memory
//! store, a locked-down host), so it needs no gate.

mod server;

use std::io::{BufRead, Write};
use std::time::Duration;

use marrow_lsp_core::mcp::DEFAULT_RUN_BUDGET;
use marrow_terminal::{Style, paint_stderr};
use serde_json::{Value as Json, json};

use server::Policy;

fn stderr_notice(label: &str) -> String {
    paint_stderr(Style::Notice, label)
}

fn stderr_error(label: &str) -> String {
    paint_stderr(Style::Error, label)
}

/// The JSON-RPC invalid-params error code, for malformed request parameters.
const INVALID_PARAMS: i64 = -32602;
/// The JSON-RPC method-not-found error code.
const METHOD_NOT_FOUND: i64 = -32601;

fn main() {
    let policy = Policy::new(data_access_enabled(), run_budget());
    // Startup is observable in the agent's stderr/output channel; note whether the
    // data tools are armed so an operator can confirm the gate's state at a glance.
    eprintln!(
        "{} starting (version {}, data tools {})",
        stderr_notice("marrow-mcp:"),
        env!("CARGO_PKG_VERSION"),
        if policy.allow_data {
            "enabled"
        } else {
            "disabled"
        }
    );
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serve(stdin.lock(), &mut out, policy);
}

/// The wall-clock budget a single `mw_run` may consume, from
/// `MARROW_MCP_RUN_BUDGET_SECS` when it names a positive whole number of seconds,
/// otherwise the default. A zero or unparseable value falls back to the default
/// so a stray env var cannot make every run fault instantly.
fn run_budget() -> Duration {
    match std::env::var("MARROW_MCP_RUN_BUDGET_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
    {
        Some(seconds) if seconds > 0 => Duration::from_secs(seconds),
        _ => DEFAULT_RUN_BUDGET,
    }
}

/// Whether the data tools may read real stored data: enabled by `--allow-data` on
/// the command line or a truthy `MARROW_MCP_ALLOW_DATA` in the environment.
/// Anything else (the default) keeps data access off.
fn data_access_enabled() -> bool {
    if std::env::args().any(|arg| arg == "--allow-data") {
        return true;
    }
    matches!(
        std::env::var("MARROW_MCP_ALLOW_DATA").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// The server loop: read one JSON-RPC message per line, handle it, and write any
/// reply as a single line. A notification (no `id`) draws no reply. Returns when
/// stdin closes (the client terminated the subprocess).
fn serve(input: impl BufRead, out: &mut impl Write, policy: Policy) {
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let message: Json = match serde_json::from_str(&line) {
            Ok(message) => message,
            // A line that is not JSON cannot be a valid MCP message; the spec says
            // not to write a non-message to stdout, and a parse error with no id
            // has nowhere to go, so it is dropped (logged to stderr).
            Err(error) => {
                eprintln!(
                    "{} ignoring unparseable line: {error}",
                    stderr_error("marrow-mcp:")
                );
                continue;
            }
        };
        if let Some(reply) = handle(&message, policy) {
            write_message(out, &reply);
        }
    }
}

/// Handle one parsed JSON-RPC message, returning the reply to send, or `None` for
/// a notification (which gets no reply). An `id` present marks a request; absent
/// marks a notification.
fn handle(message: &Json, policy: Policy) -> Option<Json> {
    let method = message.get("method").and_then(Json::as_str).unwrap_or("");
    let id = message.get("id").cloned();

    // A notification carries no id and expects no reply. `initialized` and
    // `cancelled` are the ones a client sends; any other notification is ignored.
    let id = id?;

    match method {
        "initialize" => Some(with_params(id, message, method, |id, params| {
            let requested = params.get("protocolVersion").and_then(Json::as_str);
            ok(
                id,
                server::initialize_result(server::negotiate_protocol_version(requested)),
            )
        })),
        // A liveness check the client may send; an empty result is the pong.
        "ping" => Some(with_params(id, message, method, |id, _| ok(id, json!({})))),
        "tools/list" => Some(with_params(id, message, method, |id, _| {
            ok(id, json!({ "tools": server::tools() }))
        })),
        "tools/call" => Some(with_params(id, message, method, |id, params| {
            handle_tools_call(id, &params, policy)
        })),
        // A clean shutdown handshake; reply so the client can proceed to exit.
        "shutdown" => Some(with_params(id, message, method, |id, _| ok(id, Json::Null))),
        other => Some(error(
            id,
            METHOD_NOT_FOUND,
            &format!("unknown method `{other}`"),
        )),
    }
}

fn request_params(message: &Json) -> Option<Json> {
    match message.get("params") {
        None | Some(Json::Null) => Some(json!({})),
        Some(value @ Json::Object(_)) => Some(value.clone()),
        Some(_) => None,
    }
}

fn with_params(
    id: Json,
    message: &Json,
    method: &str,
    ok_with_params: impl FnOnce(Json, Json) -> Json,
) -> Json {
    match request_params(message) {
        Some(params) => ok_with_params(id, params),
        None => error(
            id,
            INVALID_PARAMS,
            &format!("{method} `params` must be an object"),
        ),
    }
}

/// Handle a `tools/call`: pull `name` and `arguments`, dispatch to the server, and
/// wrap the tool's JSON result in the MCP content envelope. A tool that returns a
/// result (even one describing a Marrow error) is a successful call; only a
/// malformed request (no tool name, unknown tool, bad arguments) is a protocol
/// error.
fn handle_tools_call(id: Json, params: &Json, policy: Policy) -> Json {
    let Some(name) = params.get("name").and_then(Json::as_str) else {
        return error(id, INVALID_PARAMS, "tools/call needs a tool `name`");
    };
    let arguments = match params.get("arguments") {
        None | Some(Json::Null) => json!({}),
        Some(value @ Json::Object(_)) => value.clone(),
        Some(_) => {
            return error(
                id,
                INVALID_PARAMS,
                "tools/call `arguments` must be an object when present",
            );
        }
    };
    match server::call(name, &arguments, policy) {
        Ok(result) => ok(id, tool_result(name, &result)),
        Err(message) => error(id, INVALID_PARAMS, &message),
    }
}

/// Wrap a tool's JSON result in the MCP `tools/call` result envelope: a concise
/// one-line human-readable summary as a text content block (for clients that read
/// text and to keep the agent's context small) plus the full value as
/// `structuredContent` (for clients that read structured output). `isError` is
/// true only when the core marked the result a tool-internal fault (a transport,
/// IO, project-load, or resource-limit failure); a tool that ran and reports a
/// Marrow diagnostic or a domain operation error is a successful call, so an agent
/// keying on `isError` sees real faults but not a program that failed to check.
fn tool_result(name: &str, result: &Json) -> Json {
    json!({
        "content": [{ "type": "text", "text": summarize(name, result) }],
        "structuredContent": result,
        "isError": is_tool_error(result),
    })
}

/// Whether the core flagged this result a tool-internal fault (see [`tool_result`]).
fn is_tool_error(result: &Json) -> bool {
    result.get("tool_error").and_then(Json::as_bool) == Some(true)
}

/// A concise, single-line summary of a tool result for the text content block.
/// The full result always rides along in `structuredContent`, so this line only
/// needs to orient the agent — a count, a value, an outcome — at a fraction of the
/// context cost of the pretty-printed JSON. It is panic-free: every field is read
/// through `.get` with a default, and an unrecognized shape lands on `"ok"`.
fn summarize(name: &str, result: &Json) -> String {
    // A surfaced refusal or fault speaks for itself, regardless of which tool ran.
    let base = if result.get("dataAccess").and_then(Json::as_str) == Some("disabled") {
        "data access disabled".to_string()
    } else if let Some(error) = error_summary(result) {
        error
    } else {
        match name {
            "mw_check" => count_summary(result, "diagnostics", "no diagnostics", "diagnostic"),
            "mw_type_at" => match result.get("type").and_then(Json::as_str) {
                Some(ty) => clip(ty),
                None => "no type".to_string(),
            },
            "mw_complete" => count_summary(result, "items", "no completions", "completion"),
            "mw_namespace_complete" => namespace_complete_summary(result),
            "mw_resource_schema" => count_summary(result, "resources", "no resources", "resource"),
            "mw_surface_routes" => count_summary(result, "routes", "no routes", "route"),
            "mw_surface_read" | "mw_surface_write" => surface_read_summary(result),
            "mw_saved_roots" => {
                if result.get("available").and_then(Json::as_bool) != Some(true) {
                    "data unavailable".to_string()
                } else {
                    count_summary(result, "roots", "no saved roots", "saved root")
                }
            }
            "mw_data_children" => {
                if result.get("available").and_then(Json::as_bool) != Some(true) {
                    "data unavailable".to_string()
                } else {
                    let count = array_len(result, "children");
                    let base = match count {
                        0 => "no children".to_string(),
                        1 => "1 child".to_string(),
                        _ => format!("{count} children"),
                    };
                    if result.get("truncated").and_then(Json::as_bool) == Some(true) {
                        format!("{base} (truncated)")
                    } else {
                        base
                    }
                }
            }
            "mw_data_read" => {
                if result.get("available").and_then(Json::as_bool) != Some(true) {
                    "data unavailable".to_string()
                } else if let Some(value) = result.get("value").and_then(Json::as_str) {
                    let base = clip(value);
                    if result.get("value_truncated").and_then(Json::as_bool) == Some(true) {
                        format!("{base} (truncated)")
                    } else {
                        base
                    }
                } else {
                    result
                        .get("presence")
                        .and_then(Json::as_str)
                        .map(clip)
                        .unwrap_or_else(|| "absent".to_string())
                }
            }
            "mw_run" => run_summary(result),
            _ => "ok".to_string(),
        }
    };
    prefix_contract(result, base)
}

fn surface_read_summary(result: &Json) -> String {
    if result.get("available").and_then(Json::as_bool) != Some(true) {
        return "data unavailable".to_string();
    }
    result
        .get("response")
        .and_then(|response| response.get("result"))
        .and_then(|operation_result| operation_result.get("kind"))
        .and_then(Json::as_str)
        .map(clip)
        .unwrap_or_else(|| "ok".to_string())
}

fn error_summary(result: &Json) -> Option<String> {
    let error = result.get("error")?;
    let detail = error
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| compact(error));
    Some(format!("error: {}", clip(&detail)))
}

fn prefix_contract(result: &Json, base: String) -> String {
    let Some(contract) = result.get("contract") else {
        return base;
    };
    let Some(description) = contract.get("description").and_then(Json::as_str) else {
        return base;
    };
    let Some(status) = contract.get("status").and_then(Json::as_str) else {
        return format!("{}: {base}", clip(description));
    };
    let missing = contract
        .get("missingFacts")
        .and_then(Json::as_array)
        .and_then(|facts| missing_fact_summary(facts));
    match missing {
        Some(missing) => format!(
            "{} ({}: {}): {base}",
            clip(description),
            clip(status),
            missing
        ),
        None => format!("{} ({}): {base}", clip(description), clip(status)),
    }
}

fn missing_fact_summary(facts: &[Json]) -> Option<String> {
    let first = facts.first()?.as_str()?;
    if facts.len() == 1 {
        return Some(clip(first));
    }
    Some(format!("{} +{}", clip(first), facts.len() - 1))
}

/// Summarize `mw_run`: test mode carries a `tests` array (pass/fail counts), run
/// mode carries Marrow's typed run `result` plus captured `output`. Either mode
/// degrades to its `diagnostics` when the program did not check or run.
fn run_summary(result: &Json) -> String {
    // Diagnostics come first: a non-empty list means the program never produced a
    // value or a test outcome — it failed to check, or test discovery faulted,
    // which leaves an empty `tests` array beside the diagnostics. Reporting tests
    // first would dress that failure up as a clean "0/0 tests passed".
    if array_len(result, "diagnostics") > 0 {
        let count = array_len(result, "diagnostics");
        let first = result
            .get("diagnostics")
            .and_then(Json::as_array)
            .and_then(|d| d.first())
            .and_then(|d| d.get("message"))
            .and_then(Json::as_str)
            .unwrap_or("see diagnostics");
        return format!(
            "{count} diagnostic{}: {}",
            plural(count as u64),
            clip(first)
        );
    }
    if let Some(tests) = result.get("tests").and_then(Json::as_array) {
        let total = tests.len();
        let passed = tests
            .iter()
            .filter(|t| t.get("outcome").and_then(Json::as_str) == Some("passed"))
            .count();
        return format!("{passed}/{total} tests passed");
    }
    match result.get("result").and_then(Json::as_object) {
        Some(result) if result.get("kind").and_then(Json::as_str) == Some("value") => {
            let value = result.get("value").unwrap_or(&Json::Null);
            format!("value {}", clip(&run_return_summary(value)))
        }
        _ => {
            if has_output(result) {
                "ran, no value (output captured)".to_string()
            } else {
                "ran, no value".to_string()
            }
        }
    }
}

fn run_return_summary(value: &Json) -> String {
    let Some(kind) = value.get("kind").and_then(Json::as_str) else {
        return compact(value);
    };
    match kind {
        "int" | "bool" | "string" | "decimal" | "date" | "duration" | "instant" => value
            .get("value")
            .map(compact)
            .unwrap_or_else(|| compact(value)),
        "bytes" => value
            .get("originalBytes")
            .and_then(Json::as_u64)
            .map(|bytes| format!("{bytes} bytes"))
            .unwrap_or_else(|| compact(value)),
        "sequence" => {
            let count = value
                .get("values")
                .and_then(Json::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            format!("{count} values")
        }
        _ => compact(value),
    }
}

/// Whether a run captured any `print`/`write` output, so a value-less run still
/// reports that it produced something.
fn has_output(result: &Json) -> bool {
    result
        .get("output")
        .and_then(Json::as_str)
        .is_some_and(|output| !output.is_empty())
}

/// `"3 things"` / `"1 thing"` / the empty phrase, from an array field's length.
fn count_summary(result: &Json, field: &str, empty: &str, noun: &str) -> String {
    let count = array_len(result, field);
    if count == 0 {
        empty.to_string()
    } else {
        format!("{count} {noun}{}", plural(count as u64))
    }
}

fn namespace_complete_summary(result: &Json) -> String {
    match result.get("kind").and_then(Json::as_str) {
        Some("module") => {
            let count = array_len(result, "resources")
                + array_len(result, "enums")
                + array_len(result, "functions");
            let module = result
                .get("module")
                .and_then(Json::as_str)
                .unwrap_or("module");
            if count == 0 {
                format!("{module}: no namespace items")
            } else {
                format!("{module}: {count} namespace item{}", plural(count as u64))
            }
        }
        Some("enum") => {
            let enum_name = result
                .get("enum_name")
                .and_then(Json::as_str)
                .unwrap_or("enum");
            let count = array_len(result, "members");
            if count == 0 {
                format!("{enum_name}: no enum members")
            } else {
                format!("{enum_name}: {count} enum member{}", plural(count as u64))
            }
        }
        _ => "no namespace fact".to_string(),
    }
}

/// The length of an array-valued field, or 0 when it is missing or not an array.
fn array_len(result: &Json, field: &str) -> usize {
    result
        .get(field)
        .and_then(Json::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

/// `"s"` for a plural count, the empty string for one.
fn plural(count: u64) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// The first line of a string, so a multi-line message never breaks the one-line
/// summary contract.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// An inline fragment bounded to one short line: the first line, trimmed to a
/// character budget with an ellipsis, so one oversized value, type, or message
/// cannot blow the summary back up into the bulk the `content` block is meant to
/// avoid.
fn clip(text: &str) -> String {
    const MAX: usize = 120;
    let line = first_line(text);
    if line.chars().count() <= MAX {
        return line.to_string();
    }
    let kept: String = line.chars().take(MAX).collect();
    format!("{kept}…")
}

/// A compact JSON rendering of a value for an inline summary (no pretty-printing).
fn compact(value: &Json) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// A successful JSON-RPC response carrying `result`.
fn ok(id: Json, result: Json) -> Json {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A JSON-RPC error response.
fn error(id: Json, code: i64, message: &str) -> Json {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Write one JSON-RPC message as a single line (the MCP stdio framing) and flush,
/// so the client reads it promptly. `to_string` never emits a newline, satisfying
/// the "no embedded newlines" rule.
fn write_message(out: &mut impl Write, message: &Json) {
    let line = serde_json::to_string(message).expect("a JSON message serializes");
    // One message per line; flush so an interactive client is not left waiting.
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the server loop over an in-memory stdin and collect its line-framed
    /// replies as parsed JSON.
    fn drive(requests: &[Json], policy: Policy) -> Vec<Json> {
        let mut input = String::new();
        for request in requests {
            input.push_str(&serde_json::to_string(request).unwrap());
            input.push('\n');
        }
        let mut output: Vec<u8> = Vec::new();
        serve(input.as_bytes(), &mut output, policy);
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Write a minimal pure project whose `spin` entry loops forever, so a run of
    /// it can only end at its wall-clock budget.
    fn looping_project() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src = root.join("src/shelf");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("loops.mw"),
            "module shelf::loops\n\npub fn spin()\n    var i = 0\n    while true\n        i = i + 1\n",
        )
        .unwrap();
        (dir, src.join("loops.mw"))
    }

    #[test]
    fn a_looping_run_does_not_wedge_the_server() {
        // A single infinite mw_run once wedged the single-threaded server forever.
        // With a run budget the looping call must fault at the deadline and the
        // very next tool call must still be answered on the same server loop.
        let (_dir, file) = looping_project();
        let policy = Policy {
            allow_data: false,
            run_budget: std::time::Duration::from_millis(300),
        };
        let replies = drive(
            &[
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "mw_run",
                        "arguments": { "file": file.to_str().unwrap(), "entry": "shelf::loops::spin" }
                    }
                }),
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "mw_check",
                        "arguments": { "source": "module a" }
                    }
                }),
            ],
            policy,
        );
        assert_eq!(replies.len(), 2, "both calls must be answered: {replies:?}");
        let run = &replies[0]["result"]["structuredContent"];
        assert_eq!(
            run["budget_exceeded"], true,
            "the looping run must fault at the budget: {run}"
        );
        // The second call proves the server is not wedged: it answered after the
        // runaway run was detached.
        assert_eq!(replies[1]["id"], 2);
        assert_eq!(replies[1]["result"]["isError"], false, "{:?}", replies[1]);
    }

    #[test]
    fn initialize_negotiates_the_requested_protocol_version() {
        let policy = Policy::new(false, DEFAULT_RUN_BUDGET);
        let replies = drive(
            &[
                // A supported version is echoed back.
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } }),
                // A mismatched (unsupported) version falls back to the server's latest.
                json!({ "jsonrpc": "2.0", "id": 2, "method": "initialize", "params": { "protocolVersion": "2020-01-01" } }),
            ],
            policy,
        );
        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(
            replies[1]["result"]["protocolVersion"],
            server::PROTOCOL_VERSION
        );
    }

    #[test]
    fn is_error_flags_a_tool_fault_but_not_a_check_failure() {
        let policy = Policy::new(false, DEFAULT_RUN_BUDGET);
        let replies = drive(
            &[
                // An unreadable project (no marrow.json anywhere up the path) is a
                // tool-internal fault: the tool could not load or check.
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "mw_check",
                        "arguments": { "file": "/no/such/project/src/main.mw" }
                    }
                }),
                // A program that simply fails to check is a successful call: the
                // agent reads the diagnostics from the structured content.
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "mw_check",
                        "arguments": { "source": "module a\n\npub fn broken(:\n" }
                    }
                }),
            ],
            policy,
        );
        let fault = &replies[0]["result"];
        assert_eq!(
            fault["isError"], true,
            "an unreadable project is a tool fault: {fault}"
        );
        assert!(
            fault["structuredContent"]["error"].is_string(),
            "the fault carries an error string: {fault}"
        );
        let check = &replies[1]["result"];
        assert_eq!(
            check["isError"], false,
            "a program that fails to check is not a tool fault: {check}"
        );
        assert!(
            !check["structuredContent"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty(),
            "the check-failure still reports diagnostics: {check}"
        );
    }

    #[test]
    fn initialize_then_list_tools_returns_the_catalog() {
        let policy = Policy::new(false, DEFAULT_RUN_BUDGET);
        let replies = drive(
            &[
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            ],
            policy,
        );
        // The notification drew no reply, so two responses for two requests.
        assert_eq!(replies.len(), 2, "got {replies:?}");
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "marrow-mcp");
        let tools = replies[1]["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "mw_check"));
        assert!(tools.iter().any(|tool| tool["name"] == "mw_surface_write"));
    }

    #[test]
    fn tools_call_to_surface_write_refuses_when_data_access_is_disabled() {
        let policy = Policy::new(false, DEFAULT_RUN_BUDGET);
        let replies = drive(
            &[json!({
                "jsonrpc": "2.0",
                "id": 1,
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
            })],
            policy,
        );
        let result = &replies[0]["result"];
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(result["structuredContent"]["dataAccess"], "disabled");
        assert_eq!(result["structuredContent"]["contract"]["status"], "ready");
        assert_eq!(
            result["structuredContent"]["contract"]["operations"],
            "read/update/action"
        );
        assert_eq!(
            result["content"][0]["text"],
            "surface write operation (ready): data access disabled"
        );
    }

    #[test]
    fn a_tools_call_to_mw_check_returns_a_diagnostic_envelope() {
        let policy = Policy::new(false, DEFAULT_RUN_BUDGET);
        let replies = drive(
            &[json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "mw_check",
                    "arguments": { "source": "module a\n\npub fn broken(:\n" }
                }
            })],
            policy,
        );
        let result = &replies[0]["result"];
        assert_eq!(result["isError"], false);
        // The structured content carries the diagnostics array.
        let diagnostics = result["structuredContent"]["diagnostics"]
            .as_array()
            .unwrap();
        assert!(
            !diagnostics.is_empty(),
            "a malformed snippet should report a diagnostic: {result}"
        );
        // And the text block carries the concise summary, not the full JSON: the
        // malformed snippet's diagnostics read as a count, matching what the
        // structured content holds.
        let expected = summarize("mw_check", &result["structuredContent"]);
        assert!(expected.contains("diagnostic"), "{expected}");
        assert_eq!(result["content"][0]["text"], expected);
    }

    #[test]
    fn summarize_reports_a_concise_line_and_keeps_the_full_result() {
        // Each case: the tool name, its full result, and the expected one-line
        // summary. The full result must always survive in `structuredContent`.
        let cases: Vec<(&str, Json, &str)> = vec![
            ("mw_check", json!({ "diagnostics": [] }), "no diagnostics"),
            (
                "mw_check",
                json!({ "diagnostics": [{}, {}, {}] }),
                "3 diagnostics",
            ),
            ("mw_type_at", json!({ "type": "int" }), "int"),
            ("mw_type_at", json!({ "type": Json::Null }), "no type"),
            ("mw_complete", json!({ "items": [{}, {}] }), "2 completions"),
            ("mw_complete", json!({ "items": [{}] }), "1 completion"),
            (
                "mw_resource_schema",
                json!({ "resources": [{}, {}] }),
                "2 resources",
            ),
            (
                "mw_saved_roots",
                json!({ "available": true, "roots": ["a", "b"] }),
                "2 saved roots",
            ),
            (
                "mw_saved_roots",
                json!({ "available": false, "roots": [] }),
                "data unavailable",
            ),
            (
                "mw_data_children",
                json!({ "available": true, "children": [{}, {}], "truncated": false }),
                "2 children",
            ),
            (
                "mw_data_children",
                json!({ "available": true, "children": [{}], "truncated": true }),
                "1 child (truncated)",
            ),
            (
                "mw_data_children",
                json!({ "available": false, "children": [], "truncated": false }),
                "data unavailable",
            ),
            (
                "mw_data_read",
                json!({
                    "available": true,
                    "presence": "value_only",
                    "value": "42",
                    "value_truncated": false,
                }),
                "42",
            ),
            (
                "mw_data_read",
                json!({
                    "available": true,
                    "presence": "absent",
                    "value": Json::Null,
                    "value_truncated": false,
                }),
                "absent",
            ),
            (
                "mw_data_read",
                json!({
                    "available": false,
                    "presence": "absent",
                    "value": Json::Null,
                    "value_truncated": false,
                }),
                "data unavailable",
            ),
            (
                "mw_surface_routes",
                json!({ "routes": [{}, {}] }),
                "2 routes",
            ),
            (
                "mw_surface_read",
                json!({
                    "available": true,
                    "response": {
                        "result": { "kind": "record", "record": { "fields": [] } }
                    }
                }),
                "record",
            ),
            (
                "mw_surface_read",
                json!({
                    "available": true,
                    "error": {
                        "code": "surface.abi_mismatch",
                        "message": "surface operation is not active"
                    }
                }),
                "error: {\"code\":\"surface.abi_mismatch\",\"message\":\"surface operation is not active\"}",
            ),
            (
                "mw_data_children",
                json!({
                    "available": true,
                    "children": [],
                    "truncated": false,
                    "error": {
                        "kind": "path",
                        "value": { "code": "zero_limit" },
                    },
                }),
                "error: {\"kind\":\"path\",\"value\":{\"code\":\"zero_limit\"}}",
            ),
            (
                "mw_run",
                json!({
                    "result": { "kind": "value", "value": { "kind": "int", "value": 42 } },
                    "output": "",
                    "diagnostics": []
                }),
                "value 42",
            ),
            (
                "mw_run",
                json!({ "result": { "kind": "none" }, "output": "loud\n", "diagnostics": [] }),
                "ran, no value (output captured)",
            ),
            (
                "mw_run",
                json!({ "result": { "kind": "none" }, "output": "", "diagnostics": [] }),
                "ran, no value",
            ),
            (
                "mw_run",
                json!({ "diagnostics": [], "output": "", "tests": [
                    { "outcome": "passed" },
                    { "outcome": "failed" },
                    { "outcome": "passed" },
                ] }),
                "2/3 tests passed",
            ),
            (
                "mw_run",
                json!({ "diagnostics": [{ "message": "boom" }], "output": "" }),
                "1 diagnostic: boom",
            ),
            // Test discovery faulted: an empty `tests` array rides beside the
            // diagnostics, and the diagnostic — not "0/0 tests passed" — is the line.
            (
                "mw_run",
                json!({ "diagnostics": [{ "message": "no such module" }], "output": "", "tests": [] }),
                "1 diagnostic: no such module",
            ),
        ];
        for (name, result, expected) in cases {
            let summary = summarize(name, &result);
            assert_eq!(summary, expected, "{name}: {result}");
            assert!(
                summary.lines().count() <= 1,
                "summary must be one line: {summary:?}"
            );
            // The envelope keeps the full result untouched in structuredContent.
            let envelope = tool_result(name, &result);
            assert_eq!(envelope["structuredContent"], result);
            assert_eq!(envelope["content"][0]["text"], expected);
            assert_eq!(envelope["isError"], false);
        }
    }

    #[test]
    fn summarize_prefixes_contract_bearing_results_and_preserves_contract() {
        let cases: Vec<(&str, Json, &str)> = vec![
            (
                "mw_complete",
                json!({
                    "items": [{}, {}, {}],
                    "contract": {
                        "status": "ready",
                        "stableProductionApi": true,
                        "description": "source completion fact",
                        "missingFacts": [],
                    },
                }),
                "source completion fact (ready): 3 completions",
            ),
            (
                "mw_saved_roots",
                json!({
                    "available": false,
                    "roots": [],
                    "contract": {
                        "status": "ready",
                        "stableProductionApi": true,
                        "description": "saved-root listing API",
                        "missingFacts": [],
                    },
                }),
                "saved-root listing API (ready): data unavailable",
            ),
            (
                "mw_data_children",
                json!({
                    "available": true,
                    "children": [{}, {}],
                    "truncated": true,
                    "contract": {
                        "status": "ready",
                        "stableProductionApi": true,
                        "description": "bounded typed data children API",
                        "missingFacts": [],
                    },
                }),
                "bounded typed data children API (ready): 2 children (truncated)",
            ),
            (
                "mw_data_read",
                json!({
                    "available": true,
                    "presence": "value_only",
                    "value": "42",
                    "value_truncated": false,
                    "contract": {
                        "status": "ready",
                        "stableProductionApi": true,
                        "description": "bounded typed data read API",
                        "missingFacts": [],
                    },
                }),
                "bounded typed data read API (ready): 42",
            ),
            (
                "mw_surface_read",
                json!({
                    "available": true,
                    "response": { "result": { "kind": "page", "page": { "rows": [], "next": null } } },
                    "contract": json!({
                        "status": "ready",
                        "stableProductionApi": true,
                        "description": "surface read operation",
                        "missingFacts": [],
                    }),
                }),
                "surface read operation (ready): page",
            ),
            (
                "mw_run",
                json!({
                    "result": { "kind": "value", "value": { "kind": "int", "value": 42 } },
                    "output": "",
                    "diagnostics": [],
                    "contract": {
                        "status": "ready",
                        "stableProductionApi": true,
                        "description": "sandboxed execution API",
                        "missingFacts": [],
                    },
                }),
                "sandboxed execution API (ready): value 42",
            ),
        ];
        for (name, result, expected) in cases {
            let envelope = tool_result(name, &result);
            assert_eq!(envelope["content"][0]["text"], expected, "{name}");
            assert_eq!(envelope["structuredContent"], result, "{name}");
            assert_eq!(
                envelope["structuredContent"]["contract"],
                result["contract"]
            );
        }
    }

    #[test]
    fn summarize_surfaces_an_error_envelope() {
        let result = json!({ "diagnostics": [], "error": "the file path is not absolute" });
        assert_eq!(
            summarize("mw_check", &result),
            "error: the file path is not absolute"
        );
    }

    #[test]
    fn summarize_reports_the_data_disabled_envelope() {
        let result = json!({
            "available": false,
            "dataAccess": "disabled",
            "message": "data access not enabled; relaunch ...",
        });
        for tool in [
            "mw_saved_roots",
            "mw_data_children",
            "mw_surface_read",
            "mw_surface_write",
        ] {
            assert_eq!(summarize(tool, &result), "data access disabled");
        }
    }

    #[test]
    fn summarize_falls_back_to_ok_for_an_unknown_shape() {
        assert_eq!(summarize("mw_mystery", &json!({ "weird": 1 })), "ok");
    }

    #[test]
    fn supported_methods_reject_concrete_non_object_params() {
        let policy = Policy::new(false, DEFAULT_RUN_BUDGET);
        let cases = [
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": [] }),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "ping", "params": "bad" }),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": 1 }),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "shutdown", "params": false }),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": [] }),
        ];

        for request in cases {
            let replies = drive(&[request], policy);
            assert_eq!(replies.len(), 1, "got {replies:?}");
            assert!(
                replies[0].get("result").is_none(),
                "invalid params responses must not include a result: {replies:?}"
            );
            let error = &replies[0]["error"];
            assert_eq!(error["code"], INVALID_PARAMS, "{replies:?}");
            let message = error["message"].as_str().unwrap();
            assert!(message.contains("`params`"), "{message}");
        }
    }

    #[test]
    fn request_params_null_absent_and_precedence_guards() {
        let policy = Policy::new(false, DEFAULT_RUN_BUDGET);
        let replies = drive(
            &[
                json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
                json!({ "jsonrpc": "2.0", "id": 2, "method": "ping", "params": null }),
                json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": null }),
                json!({ "jsonrpc": "2.0", "id": 4, "method": "shutdown", "params": null }),
                json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": null }),
                json!({ "jsonrpc": "2.0", "id": 6, "method": "frobnicate", "params": [] }),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized", "params": [] }),
            ],
            policy,
        );

        assert_eq!(
            replies.len(),
            6,
            "the malformed notification should draw no reply: {replies:?}"
        );
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "marrow-mcp");
        assert_eq!(replies[1]["result"], json!({}));
        assert!(replies[2]["result"]["tools"].is_array(), "{replies:?}");
        assert_eq!(replies[3]["result"], Json::Null);

        let tool_call_message = replies[4]["error"]["message"].as_str().unwrap();
        assert_eq!(replies[4]["error"]["code"], INVALID_PARAMS);
        assert!(tool_call_message.contains("`name`"), "{tool_call_message}");
        assert!(
            !tool_call_message.contains("`params`"),
            "{tool_call_message}"
        );

        assert_eq!(replies[5]["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn tools_call_rejects_concrete_non_object_params_and_arguments() {
        let policy = Policy::new(false, DEFAULT_RUN_BUDGET);
        let request = |params: Json| json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params });
        let mw_check = |arguments: Json| json!({ "name": "mw_check", "arguments": arguments });
        let cases = [
            (request(json!([])), "`params`"),
            (request(json!("mw_check")), "`params`"),
            (request(json!(1)), "`params`"),
            (request(json!(false)), "`params`"),
            (request(mw_check(json!([]))), "`arguments`"),
            (request(mw_check(json!("source"))), "`arguments`"),
            (request(mw_check(json!(1))), "`arguments`"),
            (request(mw_check(json!(false))), "`arguments`"),
        ];

        for (request, field) in cases {
            let replies = drive(&[request], policy);
            let error = &replies[0]["error"];
            assert_eq!(error["code"], INVALID_PARAMS);
            assert!(
                error["message"].as_str().unwrap().contains(field),
                "error should name malformed {field}: {error}"
            );
        }
    }

    #[test]
    fn tools_call_null_params_and_arguments_remain_absent() {
        let policy = Policy::new(false, DEFAULT_RUN_BUDGET);

        let replies = drive(
            &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": null })],
            policy,
        );
        let message = replies[0]["error"]["message"].as_str().unwrap();
        assert_eq!(replies[0]["error"]["code"], INVALID_PARAMS);
        assert!(message.contains("`name`"), "{message}");
        assert!(!message.contains("`params`"), "{message}");

        for params in [
            json!({ "name": "mw_check" }),
            json!({ "name": "mw_check", "arguments": null }),
        ] {
            let replies = drive(
                &[json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": params,
                })],
                policy,
            );
            let message = replies[0]["error"]["message"].as_str().unwrap();
            assert_eq!(replies[0]["error"]["code"], INVALID_PARAMS);
            assert!(
                message.contains("`file`") && message.contains("`source`"),
                "{message}"
            );
            assert!(!message.contains("`arguments`"), "{message}");
        }

        let replies = drive(
            &[json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "mw_check",
                    "arguments": { "source": "module a" },
                },
            })],
            policy,
        );
        assert_eq!(replies[0]["result"]["isError"], false, "{replies:?}");
    }

    #[test]
    fn an_unknown_method_is_a_method_not_found_error() {
        let policy = Policy::new(false, DEFAULT_RUN_BUDGET);
        let replies = drive(
            &[json!({ "jsonrpc": "2.0", "id": 7, "method": "frobnicate" })],
            policy,
        );
        assert_eq!(replies[0]["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn a_notification_draws_no_reply() {
        let policy = Policy::new(false, DEFAULT_RUN_BUDGET);
        let replies = drive(
            &[json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })],
            policy,
        );
        assert!(
            replies.is_empty(),
            "a notification must not be answered: {replies:?}"
        );
    }
}
