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
        Ok(result) => ok(id, tool_result(&result)),
        Err(message) => error(id, INVALID_PARAMS, &message),
    }
}

/// Wrap a tool's JSON result in the MCP `tools/call` result envelope: the
/// serialized JSON as a text content block (for clients that read text) plus the
/// same value as `structuredContent` (for clients that read structured output).
/// `isError` is false — a tool that ran is a successful call even when its result
/// reports a Marrow diagnostic; the agent reads that from the structured content.
fn tool_result(result: &Json) -> Json {
    let text = serde_json::to_string_pretty(result).unwrap_or_else(|_| "{}".to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": result,
        "isError": false,
    })
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
        // And the text block mirrors it for text-only clients.
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("diagnostics")
        );
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
