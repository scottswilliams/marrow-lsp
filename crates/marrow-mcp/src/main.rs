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
//! The data tools (`mw_saved_*`) read a project's real stored data, so they are a
//! data-exfiltration boundary. They are disabled unless the operator opts in at
//! launch with `MARROW_MCP_ALLOW_DATA=1` (or the `--allow-data` flag). When
//! disabled, those tools return a clear refusal and read nothing. Execution
//! (`mw_run`) is always sandboxed (a fresh in-memory store, a locked-down host),
//! so it needs no gate.

mod server;

use std::io::{BufRead, Write};

use serde_json::{Value as Json, json};

use server::Policy;

/// The JSON-RPC invalid-params error code, for a `tools/call` whose arguments do
/// not parse.
const INVALID_PARAMS: i64 = -32602;
/// The JSON-RPC method-not-found error code.
const METHOD_NOT_FOUND: i64 = -32601;

fn main() {
    let policy = Policy {
        allow_data: data_access_enabled(),
    };
    // Startup is observable in the agent's stderr/output channel; note whether the
    // data tools are armed so an operator can confirm the gate's state at a glance.
    eprintln!(
        "marrow-mcp: starting (version {}, data tools {})",
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
                eprintln!("marrow-mcp: ignoring unparseable line: {error}");
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

    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    match method {
        "initialize" => Some(ok(id, server::initialize_result())),
        // A liveness check the client may send; an empty result is the pong.
        "ping" => Some(ok(id, json!({}))),
        "tools/list" => Some(ok(id, json!({ "tools": server::tools() }))),
        "tools/call" => Some(handle_tools_call(id, &params, policy)),
        // A clean shutdown handshake; reply so the client can proceed to exit.
        "shutdown" => Some(ok(id, Json::Null)),
        other => Some(error(
            id,
            METHOD_NOT_FOUND,
            &format!("unknown method `{other}`"),
        )),
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
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match server::call(name, &arguments, policy) {
        Ok(result) => ok(id, tool_result(name, &result)),
        Err(message) => error(id, INVALID_PARAMS, &message),
    }
}

/// Wrap a tool's JSON result in the MCP `tools/call` result envelope: a concise
/// one-line human-readable summary as a text content block (for clients that read
/// text and to keep the agent's context small) plus the full value as
/// `structuredContent` (for clients that read structured output). `isError` is
/// false — a tool that ran is a successful call even when its result reports a
/// Marrow diagnostic; the agent reads the detail from the structured content.
fn tool_result(name: &str, result: &Json) -> Json {
    json!({
        "content": [{ "type": "text", "text": summarize(name, result) }],
        "structuredContent": result,
        "isError": false,
    })
}

/// A concise, single-line summary of a tool result for the text content block.
/// The full result always rides along in `structuredContent`, so this line only
/// needs to orient the agent — a count, a value, an outcome — at a fraction of the
/// context cost of the pretty-printed JSON. It is panic-free: every field is read
/// through `.get` with a fallback, and an unrecognized shape lands on `"ok"`.
fn summarize(name: &str, result: &Json) -> String {
    // A surfaced refusal or fault speaks for itself, regardless of which tool ran.
    let base = if result.get("dataAccess").and_then(Json::as_str) == Some("disabled") {
        "data access disabled".to_string()
    } else if let Some(error) = result.get("error").and_then(Json::as_str) {
        format!("error: {}", clip(error))
    } else {
        match name {
            "mw_check" => count_summary(result, "diagnostics", "no diagnostics", "diagnostic"),
            "mw_type_at" => match result.get("type").and_then(Json::as_str) {
                Some(ty) => clip(ty),
                None => "no type".to_string(),
            },
            "mw_complete" => count_summary(result, "items", "no completions", "completion"),
            "mw_resource_schema" => count_summary(result, "resources", "no resources", "resource"),
            "mw_saved_roots" => {
                if result.get("available").and_then(Json::as_bool) != Some(true) {
                    "data unavailable".to_string()
                } else {
                    count_summary(result, "roots", "no saved roots", "saved root")
                }
            }
            "mw_saved_get" => {
                if result.get("available").and_then(Json::as_bool) != Some(true) {
                    "not present".to_string()
                } else {
                    match result.get("value") {
                        Some(value) => format!("value {}", clip(&compact(value))),
                        // No value rendered: a record node that is absent or holds only
                        // children carries its `presence` word, which is the honest line.
                        None => match result.get("presence").and_then(Json::as_str) {
                            Some(presence) => clip(presence),
                            None => "present".to_string(),
                        },
                    }
                }
            }
            "mw_saved_children" => {
                if result.get("available").and_then(Json::as_bool) != Some(true) {
                    "data unavailable".to_string()
                } else {
                    let children = array_len(result, "children");
                    let base = match children {
                        0 => "no children".to_string(),
                        1 => "1 child".to_string(),
                        n => format!("{n} children"),
                    };
                    if result.get("more").and_then(Json::as_bool) == Some(true) {
                        format!("{base} (more)")
                    } else {
                        base
                    }
                }
            }
            "mw_data_integrity" => {
                if result.get("available").and_then(Json::as_bool) != Some(true) {
                    "data unavailable".to_string()
                } else {
                    let scanned = result.get("scanned").and_then(Json::as_u64).unwrap_or(0);
                    let findings = array_len(result, "findings");
                    let base = if findings == 0 {
                        format!("clean, {scanned} scanned")
                    } else {
                        format!(
                            "{findings} finding{}, {scanned} scanned",
                            plural(findings as u64)
                        )
                    };
                    if result.get("truncated").and_then(Json::as_bool) == Some(true) {
                        format!("{base} (truncated)")
                    } else {
                        base
                    }
                }
            }
            "mw_run" => run_summary(result),
            _ => "ok".to_string(),
        }
    };
    prefix_contract(result, base)
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
/// mode carries a `value`/`output`. Either mode degrades to its `diagnostics` when
/// the program did not check or run.
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
    match result.get("value") {
        Some(Json::Null) | None => {
            if has_output(result) {
                "ran, no value (output captured)".to_string()
            } else {
                "ran, no value".to_string()
            }
        }
        Some(value) => format!("value {}", clip(&compact(value))),
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

    #[test]
    fn initialize_then_list_tools_returns_the_catalog() {
        let policy = Policy { allow_data: false };
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
    }

    #[test]
    fn a_tools_call_to_mw_check_returns_a_diagnostic_envelope() {
        let policy = Policy { allow_data: false };
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
                "mw_saved_get",
                json!({ "available": true, "value": "\"Mort\"", "type": "string" }),
                "value \"\\\"Mort\\\"\"",
            ),
            ("mw_saved_get", json!({ "available": false }), "not present"),
            (
                "mw_saved_children",
                json!({ "available": true, "children": [{}, {}], "more": true }),
                "2 children (more)",
            ),
            (
                "mw_data_integrity",
                json!({ "available": true, "findings": [], "scanned": 5, "truncated": false }),
                "clean, 5 scanned",
            ),
            (
                "mw_data_integrity",
                json!({ "available": true, "findings": [{}], "scanned": 9, "truncated": false }),
                "1 finding, 9 scanned",
            ),
            (
                "mw_run",
                json!({ "value": 42, "output": "", "diagnostics": [] }),
                "value 42",
            ),
            (
                "mw_run",
                json!({ "value": Json::Null, "output": "loud\n", "diagnostics": [] }),
                "ran, no value (output captured)",
            ),
            (
                "mw_run",
                json!({ "value": Json::Null, "output": "", "diagnostics": [] }),
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
            // An available saved path with no rendered value reports its presence
            // word rather than implying a value is present.
            (
                "mw_saved_get",
                json!({ "available": true, "presence": "absent" }),
                "absent",
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
        let contract = |status: &str, description: &str, missing_facts: &[&str]| {
            json!({
                "status": status,
                "stableProductionApi": false,
                "description": description,
                "missingFacts": missing_facts,
            })
        };
        let cases: Vec<(&str, Json, &str)> = vec![
            (
                "mw_complete",
                json!({
                    "items": [{}, {}, {}],
                    "contract": contract(
                        "blocked-on-marrow",
                        "development helper",
                        &["canonical completion-context facts"],
                    ),
                }),
                "development helper (blocked-on-marrow: canonical completion-context facts): 3 completions",
            ),
            (
                "mw_saved_children",
                json!({
                    "available": true,
                    "children": [{}, {}],
                    "more": false,
                    "contract": contract(
                        "blocked-on-marrow",
                        "debug/admin prototype",
                        &[
                            "catalog-bound saved-place identity",
                            "typed children",
                            "cursor/page facts",
                        ],
                    ),
                }),
                "debug/admin prototype (blocked-on-marrow: catalog-bound saved-place identity +2): 2 children",
            ),
            (
                "mw_saved_roots",
                json!({
                    "available": false,
                    "roots": [],
                    "contract": contract(
                        "blocked-on-marrow",
                        "debug/admin prototype",
                        &["catalog-bound saved-place identity"],
                    ),
                }),
                "debug/admin prototype (blocked-on-marrow: catalog-bound saved-place identity): data unavailable",
            ),
            (
                "mw_data_integrity",
                json!({
                    "available": true,
                    "findings": [],
                    "scanned": 10,
                    "truncated": false,
                    "contract": contract(
                        "presentation-only",
                        "debug/admin advisory",
                        &[
                            "catalog/store identity",
                            "store generation",
                            "catalog epoch/digest",
                            "typed repair or drift facts",
                        ],
                    ),
                }),
                "debug/admin advisory (presentation-only: catalog/store identity +3): clean, 10 scanned",
            ),
            (
                "mw_run",
                json!({
                    "value": 42,
                    "output": "",
                    "diagnostics": [],
                    "contract": contract(
                        "presentation-only",
                        "debug/admin prototype",
                        &[
                            "transitive effect facts",
                            "durable-scope facts",
                            "transaction facts",
                        ],
                    ),
                }),
                "debug/admin prototype (presentation-only: transitive effect facts +2): value 42",
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
            "mw_saved_get",
            "mw_saved_children",
            "mw_data_integrity",
        ] {
            assert_eq!(summarize(tool, &result), "data access disabled");
        }
    }

    #[test]
    fn summarize_falls_back_to_ok_for_an_unknown_shape() {
        assert_eq!(summarize("mw_mystery", &json!({ "weird": 1 })), "ok");
    }

    #[test]
    fn an_unknown_method_is_a_method_not_found_error() {
        let policy = Policy { allow_data: false };
        let replies = drive(
            &[json!({ "jsonrpc": "2.0", "id": 7, "method": "frobnicate" })],
            policy,
        );
        assert_eq!(replies[0]["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn a_notification_draws_no_reply() {
        let policy = Policy { allow_data: false };
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
