//! The MCP request dispatcher: the tool catalog and the `tools/call` routing.
//!
//! Every handler is one call into [`marrow_lsp_core::mcp`] plus argument parsing —
//! no analysis lives here. The catalog ([`tools`]) is the JSON Schema the client
//! shows an agent; [`call`] decodes a `tools/call`'s arguments and returns the
//! tool's JSON result, which the transport wraps in the MCP content envelope.

use std::path::PathBuf;

use marrow_lsp_core::mcp::{self, RunArgsPolicy, RunMode};
use serde_json::{Value as Json, json};

/// The MCP protocol version this server speaks, echoed in the `initialize` reply.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The server's runtime policy: whether the data tools may read real stored data.
/// Set once at launch from the environment / a flag and never mutated, so a tool
/// can never escalate its own access mid-session.
#[derive(Clone, Copy)]
pub struct Policy {
    pub allow_data: bool,
}

/// The `serverInfo` block of the `initialize` reply.
pub fn server_info() -> Json {
    json!({ "name": "marrow-mcp", "version": env!("CARGO_PKG_VERSION") })
}

/// The result of an `initialize` request: the protocol version, the server's
/// capabilities (it serves tools), and its identity. Capabilities advertise only
/// tools — this server exposes no resources or prompts.
pub fn initialize_result() -> Json {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": server_info(),
    })
}

/// A required string property in a tool's input schema.
fn string_prop(description: &str) -> Json {
    json!({ "type": "string", "description": description })
}

/// An integer property (a zero-based line/character).
fn int_prop(description: &str) -> Json {
    json!({ "type": "integer", "description": description })
}

/// The tool catalog: one entry per agent tool, each with the JSON Schema for its
/// arguments. This is the smallest set that closes an agent's `.mw` loop — check,
/// inspect a type, complete, read the schema, inspect saved data (gated), and run.
pub fn tools() -> Json {
    json!([
        {
            "name": "mw_check",
            "description": "Check Marrow source and return diagnostics. Pass `file` (a path inside a project) to type-check the whole project — optionally with `source` to overlay an unsaved edit — or pass a bare `source` snippet for syntax-only checking. This is the primary compile-error feedback tool.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to a .mw file inside a Marrow project."),
                    "source": string_prop("The .mw source to check; overlays `file` when both are given, or is checked standalone (syntax only) when `file` is omitted."),
                },
            },
        },
        {
            "name": "mw_type_at",
            "description": "Report the Marrow type of the expression at a zero-based UTF-16 line/character in `file`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to a .mw file inside a Marrow project."),
                    "line": int_prop("Zero-based line."),
                    "character": int_prop("Zero-based UTF-16 character offset on the line."),
                },
                "required": ["file", "line", "character"],
            },
        },
        {
            "name": "mw_complete",
            "description": "List context-aware completion items (in-scope names, resource fields, saved roots, std ops, keywords) at a position in `file`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to a .mw file inside a Marrow project."),
                    "line": int_prop("Zero-based line."),
                    "character": int_prop("Zero-based UTF-16 character offset on the line."),
                },
                "required": ["file", "line", "character"],
            },
        },
        {
            "name": "mw_resource_schema",
            "description": "Return one resource schema by `name`, or every resource in the project when `name` is omitted: the saved root, identity keys, member tree (fields, keyed leaves, groups), and indexes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "name": string_prop("The resource name to project; omit to list all resources."),
                },
                "required": ["file"],
            },
        },
        {
            "name": "mw_saved_roots",
            "description": "Debug/admin prototype: list raw saved-data root names from the project's real store. Requires data access to be enabled at launch; otherwise returns a refusal envelope and reads nothing. This is not a stable typed production API.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                },
                "required": ["file"],
            },
        },
        {
            "name": "mw_saved_get",
            "description": "Debug/admin prototype: read raw saved-data presence/value at a saved `path` from the project's real store. Gated behind data access. The path is a raw segment array: {root}/{key}/{field}/{layer}/{index}/{index_key}; a key is {int}/{str}/{bool}/{date}/{duration}/{instant}/{bytes}. This is not a stable typed production API.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "path": { "type": "array", "description": "Debug/admin prototype raw saved-data path segments, root-first; not a stable typed production API." },
                },
                "required": ["file", "path"],
            },
        },
        {
            "name": "mw_saved_children",
            "description": "Debug/admin prototype: list the immediate raw saved-data children of a saved `path` from the project's real store, capped. Gated behind data access. Same raw path encoding as mw_saved_get. This is not a stable typed production API.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "path": { "type": "array", "description": "Debug/admin prototype raw saved-data path segments, root-first; empty array lists the roots' level; not a stable typed production API." },
                },
                "required": ["file", "path"],
            },
        },
        {
            "name": "mw_data_integrity",
            "description": "Scan the project's real stored data against the CURRENT schema and report every saved record the schema can no longer account for: an orphan path (a root or member the schema no longer declares) or a value that no longer decodes as its declared type. The capped, on-demand schema-change-impact advisory — the same check `marrow data integrity` runs. Gated behind data access; returns a refusal envelope and reads nothing when disabled.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                },
                "required": ["file"],
            },
        },
        {
            "name": "mw_run",
            "description": "Debug/admin prototype for execution/args: execute Marrow to confirm behavior, always sandboxed over a FRESH in-memory store under a locked-down host (fixed clock + captured log, no filesystem/env/maintenance) — the project's real store is never touched. `mode: \"run\"` evaluates `entry` (\"module::fn\"). Non-empty `args` are blocked by default until Marrow exposes typed run argument facts; set `allowPrototypeArgs: true` only to use the debug/admin prototype scalar decoder. `args` are not a stable typed production API. `mode: \"test\"` runs the project's test suite, each test over its own fresh store.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "entry": string_prop("The function to run as `module::fn` (run mode)."),
                    "args": { "type": "array", "description": "Debug/admin prototype positional scalar arguments for run mode; non-empty args require `allowPrototypeArgs: true` and are not a stable typed production API." },
                    "allowPrototypeArgs": { "type": "boolean", "description": "Set true to enable the debug/admin prototype scalar decoder for non-empty `args`; omit for the stable default." },
                    "mode": { "type": "string", "enum": ["run", "test"], "description": "`run` (default) executes `entry`; `test` runs the project's tests." },
                },
                "required": ["file"],
            },
        },
    ])
}

/// Route a `tools/call` to its core handler, returning the tool's JSON result.
/// `Err` carries a message for an invalid-arguments protocol error (a missing
/// required argument or an unknown tool); `Ok` carries the tool's own result,
/// which the transport renders into the MCP content envelope.
pub fn call(name: &str, arguments: &Json, policy: Policy) -> Result<Json, String> {
    match name {
        "mw_check" => {
            let file = optional_path(arguments, "file");
            let source = optional_str(arguments, "source");
            if file.is_none() && source.is_none() {
                return Err("mw_check needs a `file` or a `source`".to_string());
            }
            Ok(mcp::check(file.as_deref(), source))
        }
        "mw_type_at" => {
            let file = required_path(arguments, "file")?;
            let line = required_u32(arguments, "line")?;
            let character = required_u32(arguments, "character")?;
            Ok(mcp::type_at_position(&file, line, character))
        }
        "mw_complete" => {
            let file = required_path(arguments, "file")?;
            let line = required_u32(arguments, "line")?;
            let character = required_u32(arguments, "character")?;
            Ok(mcp::complete(&file, line, character))
        }
        "mw_resource_schema" => {
            let file = required_path(arguments, "file")?;
            let name = optional_str(arguments, "name");
            Ok(mcp::resource_schema(&file, name))
        }
        "mw_saved_roots" => {
            let file = required_path(arguments, "file")?;
            Ok(mcp::saved_roots(&file, policy.allow_data))
        }
        "mw_saved_get" => {
            let file = required_path(arguments, "file")?;
            let path = arguments.get("path").cloned().unwrap_or_else(|| json!([]));
            Ok(mcp::saved_get(&file, path, policy.allow_data))
        }
        "mw_saved_children" => {
            let file = required_path(arguments, "file")?;
            let path = arguments.get("path").cloned().unwrap_or_else(|| json!([]));
            Ok(mcp::saved_children(&file, path, policy.allow_data))
        }
        "mw_data_integrity" => {
            let file = required_path(arguments, "file")?;
            Ok(mcp::data_integrity(&file, policy.allow_data))
        }
        "mw_run" => {
            let file = required_path(arguments, "file")?;
            let entry = optional_str(arguments, "entry");
            let args: Vec<Json> = arguments
                .get("args")
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default();
            let mode = match optional_str(arguments, "mode") {
                Some("test") => RunMode::Test,
                Some("run") | None => RunMode::Run,
                Some(other) => {
                    return Err(format!("unknown run mode `{other}`; use `run` or `test`"));
                }
            };
            let args_policy = if optional_bool(arguments, "allowPrototypeArgs").unwrap_or(false) {
                RunArgsPolicy::AllowPrototypeScalars
            } else {
                RunArgsPolicy::StableTypedFactsOnly
            };
            Ok(mcp::run(&file, entry, &args, mode, args_policy))
        }
        other => Err(format!("unknown tool `{other}`")),
    }
}

/// A required string argument, or an invalid-arguments message naming it.
fn required_str<'a>(arguments: &'a Json, key: &str) -> Result<&'a str, String> {
    arguments
        .get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| format!("missing or non-string argument `{key}`"))
}

/// An optional string argument, or `None` when absent or not a string.
fn optional_str<'a>(arguments: &'a Json, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(Json::as_str)
}

/// An optional boolean argument, or `None` when absent or not a boolean.
fn optional_bool(arguments: &Json, key: &str) -> Option<bool> {
    arguments.get(key).and_then(Json::as_bool)
}

/// A required path argument as a `PathBuf`.
fn required_path(arguments: &Json, key: &str) -> Result<PathBuf, String> {
    required_str(arguments, key).map(PathBuf::from)
}

/// An optional path argument as a `PathBuf`.
fn optional_path(arguments: &Json, key: &str) -> Option<PathBuf> {
    optional_str(arguments, key).map(PathBuf::from)
}

/// A required non-negative integer argument as a `u32` (a line or character).
fn required_u32(arguments: &Json, key: &str) -> Result<u32, String> {
    arguments
        .get(key)
        .and_then(Json::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("missing or out-of-range integer argument `{key}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalog_lists_every_agent_tool_with_a_schema() {
        let tools = tools();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        for expected in [
            "mw_check",
            "mw_type_at",
            "mw_complete",
            "mw_resource_schema",
            "mw_saved_roots",
            "mw_saved_get",
            "mw_saved_children",
            "mw_data_integrity",
            "mw_run",
        ] {
            assert!(
                names.contains(&expected),
                "the catalog must list {expected}, got {names:?}"
            );
        }
        // Every tool carries an object input schema.
        for tool in tools.as_array().unwrap() {
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "tool {} needs an object schema",
                tool["name"]
            );
        }
    }

    #[test]
    fn catalog_marks_saved_data_tools_as_debug_admin_prototypes() {
        let tools = tools();
        let tools = tools.as_array().unwrap();
        let saved_tool = |name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("missing saved-data tool {name}"))
        };

        for name in ["mw_saved_roots", "mw_saved_get", "mw_saved_children"] {
            let description = saved_tool(name)["description"].as_str().unwrap();
            let lower = description.to_ascii_lowercase();
            assert!(
                lower.contains("debug/admin prototype"),
                "{name} must advertise the debug/admin prototype contract: {description}"
            );
            assert!(
                lower.contains("raw saved-data"),
                "{name} must advertise raw saved-data access: {description}"
            );
            assert!(
                lower.contains("not a stable typed production api"),
                "{name} must not present a stable typed production API: {description}"
            );
            assert!(
                !lower.contains("schema-typed"),
                "{name} must not claim schema-typed saved-data semantics: {description}"
            );
        }

        for name in ["mw_saved_get", "mw_saved_children"] {
            let description = saved_tool(name)["inputSchema"]["properties"]["path"]["description"]
                .as_str()
                .unwrap();
            let lower = description.to_ascii_lowercase();
            assert!(
                lower.contains("debug/admin prototype"),
                "{name}.path must advertise the debug/admin prototype contract: {description}"
            );
            assert!(
                lower.contains("raw saved-data"),
                "{name}.path must advertise raw saved-data path segments: {description}"
            );
            assert!(
                lower.contains("not a stable typed production api"),
                "{name}.path must not present a stable typed production API: {description}"
            );
            assert!(
                !lower.contains("schema-typed"),
                "{name}.path must not claim schema-typed path semantics: {description}"
            );
        }
    }

    #[test]
    fn catalog_marks_mw_run_args_as_a_prototype_opt_in() {
        let tools = tools();
        let tools = tools.as_array().unwrap();
        let run_tool = tools
            .iter()
            .find(|tool| tool["name"] == "mw_run")
            .expect("mw_run tool");

        let description = run_tool["description"].as_str().unwrap();
        let lower = description.to_ascii_lowercase();
        assert!(
            lower.contains("debug/admin prototype"),
            "mw_run must advertise its prototype execution contract: {description}"
        );
        assert!(
            lower.contains("allowprototypeargs"),
            "mw_run must name the explicit prototype arg opt-in: {description}"
        );
        assert!(
            lower.contains("typed run argument facts"),
            "mw_run must point argument decoding back to Marrow facts: {description}"
        );

        let properties = &run_tool["inputSchema"]["properties"];
        assert!(
            properties.get("allowPrototypeArgs").is_some(),
            "mw_run schema must expose the explicit prototype arg opt-in: {properties}"
        );
        let args_description = properties["args"]["description"].as_str().unwrap();
        let lower = args_description.to_ascii_lowercase();
        assert!(
            lower.contains("debug/admin prototype"),
            "mw_run.args must not look like a stable argument protocol: {args_description}"
        );
        assert!(
            lower.contains("not a stable typed production api"),
            "mw_run.args must not claim production typing: {args_description}"
        );
    }

    #[test]
    fn an_unknown_tool_is_an_error() {
        let policy = Policy { allow_data: false };
        assert!(call("mw_nope", &json!({}), policy).is_err());
    }

    #[test]
    fn a_data_tool_refuses_when_the_policy_disallows_it() {
        let policy = Policy { allow_data: false };
        // A nonexistent file is fine: the gate is checked before any project load,
        // so the refusal envelope comes back without touching the filesystem.
        let result = call("mw_saved_roots", &json!({ "file": "/nope/x.mw" }), policy).unwrap();
        assert_eq!(result["dataAccess"], "disabled");
        // The schema-impact advisory reads the store, so it is gated the same way.
        let result = call(
            "mw_data_integrity",
            &json!({ "file": "/nope/x.mw" }),
            policy,
        )
        .unwrap();
        assert_eq!(result["dataAccess"], "disabled");
    }

    #[test]
    fn a_missing_required_argument_is_an_error() {
        let policy = Policy { allow_data: false };
        assert!(call("mw_type_at", &json!({ "file": "/x.mw", "line": 0 }), policy).is_err());
    }

    #[test]
    fn mw_run_blocks_non_empty_args_without_prototype_opt_in() {
        let policy = Policy { allow_data: false };
        let result = call(
            "mw_run",
            &json!({
                "file": "/nope/project/src/main.mw",
                "entry": "app::main",
                "args": [1],
            }),
            policy,
        )
        .unwrap();
        let message = result["diagnostics"][0]["message"].as_str().unwrap();
        assert!(
            message.contains("typed run argument facts from Marrow"),
            "mw_run args must be blocked before project loading without the prototype opt-in: {result}"
        );
    }

    #[test]
    fn mw_run_allows_non_empty_args_with_prototype_opt_in() {
        let policy = Policy { allow_data: false };
        let result = call(
            "mw_run",
            &json!({
                "file": "/nope/project/src/main.mw",
                "entry": "app::main",
                "args": [1],
                "allowPrototypeArgs": true,
            }),
            policy,
        )
        .unwrap();
        let message = result["diagnostics"][0]["message"].as_str().unwrap();
        assert!(
            !message.contains("typed run argument facts from Marrow"),
            "the explicit prototype opt-in should let core reach normal project loading: {result}"
        );
        assert!(
            message.contains("no marrow.json"),
            "the nonexistent project should fail after the args gate: {result}"
        );
    }

    #[test]
    fn initialize_result_advertises_tools_and_the_protocol_version() {
        let result = initialize_result();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "marrow-mcp");
    }
}
