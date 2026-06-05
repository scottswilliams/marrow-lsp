//! The MCP request dispatcher: the tool catalog and the `tools/call` routing.
//!
//! Every handler is one call into [`marrow_lsp_core::mcp`] plus argument parsing —
//! no analysis lives here. The catalog ([`tools`]) is the JSON Schema the client
//! shows an agent; [`call`] decodes a `tools/call`'s arguments and returns the
//! tool's JSON result, which the transport wraps in the MCP content envelope.

use std::path::PathBuf;

use marrow_lsp_core::mcp::{self, RunMode};
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
            "description": "Presentation-only development helper: list current context-aware completion items (in-scope names, resource fields, saved roots, std ops, keywords) at a position in `file`. Missing canonical completion-context facts for any production contract; not a stable production completion API.",
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
            "description": "Presentation-only development inspection helper over current checked schema facts: return one named resource schema, including saved root, identity keys, member tree (fields, keyed leaves, groups), and indexes. The resource `name` is required so this tool cannot materialize the whole catalog; paged catalog/schema listing waits for Marrow-owned DTOs. Missing catalog-bound resource/store/member identity, presence/default facts, and typed protocol DTOs; not a stable production schema API.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "name": string_prop("Source-level resource name for this development inspection helper; required until Marrow exposes paged catalog/schema DTOs."),
                },
                "required": ["file", "name"],
            },
        },
        {
            "name": "mw_saved_roots",
            "description": "Presentation-only root-only data helper: list saved root names from the project's real store when data access is enabled. It returns no child paths or stored values, accepts no editor-authored saved path, and reads nothing when data access is disabled. Missing catalog-bound saved-place identity, typed children, cursor/page facts, snapshot/store generation, and stable data DTOs; not a stable typed production API.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                },
                "required": ["file"],
            },
        },
        {
            "name": "mw_data_integrity",
            "description": "Debug/admin advisory over the current schema and real store: scan the project's real stored data and report capped/current-schema advisory findings such as orphan paths (roots or members the schema no longer declares) or values that no longer decode as their declared type. This is the capped, on-demand schema-change-impact advisory — the same check `marrow data integrity` runs — not complete production validation. Gated behind data access; returns a refusal envelope and reads nothing when disabled. This is not catalog-epoch/store-generation-bound production validation or repair.",
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
            "description": "Presentation-only execution helper: execute Marrow to confirm behavior, always sandboxed over a FRESH in-memory store under a locked-down host (fixed clock + captured log, no filesystem/env/maintenance) — the project's real store is never touched. Projects that need accepted catalog identity are refused because this tool will not establish durable catalog state. `mode: \"run\"` evaluates `entry` (\"module::fn\") as a presentation contract until Marrow exposes canonical function-entry facts; the entry string is not a stable production entry API. Non-empty `args` are blocked in every mode until Marrow exposes typed run argument facts. `mode: \"test\"` runs the project's test suite, each test over its own fresh store.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "entry": string_prop("Entry string such as `module::fn` (run mode); useful until Marrow exposes canonical function-entry facts, but not a stable production entry API."),
                    "args": { "type": "array", "description": "Positional arguments are not accepted yet; non-empty args are blocked in every mode until Marrow exposes typed run argument facts." },
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
            let name = required_str(arguments, "name")?;
            Ok(mcp::resource_schema(&file, name))
        }
        "mw_saved_roots" => {
            let file = required_path(arguments, "file")?;
            Ok(mcp::saved_roots(&file, policy.allow_data))
        }
        "mw_data_integrity" => {
            let file = required_path(arguments, "file")?;
            Ok(mcp::data_integrity(&file, policy.allow_data))
        }
        "mw_run" => {
            let file = required_path(arguments, "file")?;
            let entry = optional_str(arguments, "entry");
            let args = optional_array(arguments, "args")?;
            let mode = match optional_str(arguments, "mode") {
                Some("test") => RunMode::Test,
                Some("run") | None => RunMode::Run,
                Some(other) => {
                    return Err(format!("unknown run mode `{other}`; use `run` or `test`"));
                }
            };
            Ok(mcp::run(&file, entry, &args, mode))
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

/// An optional array argument, empty when absent.
fn optional_array(arguments: &Json, key: &str) -> Result<Vec<Json>, String> {
    match arguments.get(key) {
        Some(value) => value
            .as_array()
            .cloned()
            .ok_or_else(|| format!("missing or non-array argument `{key}`")),
        None => Ok(Vec::new()),
    }
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
    fn catalog_marks_saved_roots_as_root_only_presentation_helper() {
        let tools = tools();
        let tools = tools.as_array().unwrap();
        let saved_tool = |name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("missing saved-data tool {name}"))
        };

        let description = saved_tool("mw_saved_roots")["description"]
            .as_str()
            .unwrap();
        let lower = description.to_ascii_lowercase();
        assert!(
            lower.contains("presentation-only"),
            "mw_saved_roots must advertise the presentation-only contract: {description}"
        );
        assert!(
            lower.contains("root-only data helper"),
            "mw_saved_roots must advertise saved-data access: {description}"
        );
        assert!(
            lower.contains("no child paths") && lower.contains("no editor-authored saved path"),
            "mw_saved_roots must not imply saved-path traversal: {description}"
        );
        assert!(
            lower.contains("not a stable typed production api"),
            "mw_saved_roots must not present a stable typed production API: {description}"
        );
        assert!(
            !lower.contains("schema-typed"),
            "mw_saved_roots must not claim schema-typed saved-data semantics: {description}"
        );
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        let removed_get = "mw_saved_get";
        let removed_children = "mw_saved_children";
        assert!(
            !names.contains(&removed_get) && !names.contains(&removed_children),
            "saved-data tools that accept client-authored paths stay absent until Marrow exposes typed path facts"
        );
    }

    #[test]
    fn catalog_marks_active_helpers_as_presentation_only() {
        let tools = tools();
        let tools = tools.as_array().unwrap();
        let tool = |name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("missing MCP tool {name}"))
        };
        let description = |name: &str| {
            tool(name)["description"]
                .as_str()
                .unwrap_or_else(|| panic!("missing description for {name}"))
                .to_ascii_lowercase()
        };

        let complete = description("mw_complete");
        assert!(
            complete.contains("development helper"),
            "mw_complete must advertise that completion is a development helper: {complete}"
        );
        assert!(
            complete.contains("presentation-only"),
            "mw_complete must advertise presentation-only helper status: {complete}"
        );
        assert!(
            complete.contains("canonical completion-context facts"),
            "mw_complete must point to the missing Marrow completion-context facts: {complete}"
        );
        assert!(
            complete.contains("not a stable production completion api"),
            "mw_complete must not present a stable production completion API: {complete}"
        );

        let schema = description("mw_resource_schema");
        assert!(
            schema.contains("development inspection helper"),
            "mw_resource_schema must advertise inspection-helper status: {schema}"
        );
        assert!(
            schema.contains("presentation-only"),
            "mw_resource_schema must advertise presentation-only helper status: {schema}"
        );
        assert!(
            schema.contains("current checked schema facts"),
            "mw_resource_schema must say it renders current checked schema facts: {schema}"
        );
        assert!(
            schema.contains("cannot materialize the whole catalog"),
            "mw_resource_schema must not expose all-resource materialization: {schema}"
        );
        assert!(
            schema.contains("catalog-bound resource/store/member identity"),
            "mw_resource_schema must name the missing catalog-bound identities: {schema}"
        );
        assert!(
            schema.contains("presence/default facts"),
            "mw_resource_schema must name missing presence/default facts: {schema}"
        );
        assert!(
            schema.contains("typed protocol dtos"),
            "mw_resource_schema must name missing typed protocol DTOs: {schema}"
        );
        assert!(
            schema.contains("not a stable production schema api"),
            "mw_resource_schema must not present a stable production schema API: {schema}"
        );

        let schema_name =
            tool("mw_resource_schema")["inputSchema"]["properties"]["name"]["description"]
                .as_str()
                .unwrap();
        let schema_name = schema_name.to_ascii_lowercase();
        assert!(
            schema_name.contains("development inspection helper"),
            "mw_resource_schema.name must not imply a stable resource identity protocol: {schema_name}"
        );
        assert!(
            schema_name.contains("required") && schema_name.contains("paged catalog/schema dtos"),
            "mw_resource_schema.name must require a named query until paged DTOs exist: {schema_name}"
        );

        let integrity = description("mw_data_integrity");
        assert!(
            integrity.contains("debug/admin advisory"),
            "mw_data_integrity must advertise advisory debug/admin status: {integrity}"
        );
        assert!(
            !integrity.contains("every saved record"),
            "mw_data_integrity must not overclaim complete saved-record coverage: {integrity}"
        );
        assert!(
            integrity.contains("current schema") && integrity.contains("real store"),
            "mw_data_integrity must name its current-schema/real-store basis: {integrity}"
        );
        assert!(
            integrity.contains("capped/current-schema advisory findings"),
            "mw_data_integrity must describe capped current-schema advisory findings: {integrity}"
        );
        assert!(
            integrity.contains("gated behind data access"),
            "mw_data_integrity must advertise the data-access gate: {integrity}"
        );
        assert!(
            integrity.contains(
                "not catalog-epoch/store-generation-bound production validation or repair"
            ),
            "mw_data_integrity must not present production validation or repair: {integrity}"
        );
    }

    #[test]
    fn catalog_marks_mw_run_args_as_blocked() {
        let tools = tools();
        let tools = tools.as_array().unwrap();
        let run_tool = tools
            .iter()
            .find(|tool| tool["name"] == "mw_run")
            .expect("mw_run tool");

        let description = run_tool["description"].as_str().unwrap();
        let lower = description.to_ascii_lowercase();
        assert!(
            !lower.contains(&["allow", "proto", "type", "args"].concat()),
            "mw_run must not advertise an argument opt-in: {description}"
        );
        assert!(
            lower.contains("typed run argument facts"),
            "mw_run must point argument decoding back to Marrow facts: {description}"
        );
        assert!(
            lower.contains("blocked in every mode"),
            "mw_run must block non-empty args for test mode as well as run mode: {description}"
        );
        assert!(
            lower.contains("accepted catalog identity")
                && lower.contains("will not establish durable catalog state"),
            "mw_run must advertise pending catalog identity refusal: {description}"
        );

        let properties = &run_tool["inputSchema"]["properties"];
        let removed_args_opt_in = ["allow", "Prototype", "Args"].concat();
        assert!(
            properties.get(&removed_args_opt_in).is_none(),
            "mw_run schema must not expose an argument opt-in: {properties}"
        );
        let args_description = properties["args"]["description"].as_str().unwrap();
        let lower = args_description.to_ascii_lowercase();
        assert!(
            lower.contains("blocked in every mode") && lower.contains("typed run argument facts"),
            "mw_run.args must name the Marrow blocker: {args_description}"
        );
    }

    #[test]
    fn catalog_marks_mw_run_entry_as_presentation_only() {
        let tools = tools();
        let tools = tools.as_array().unwrap();
        let run_tool = tools
            .iter()
            .find(|tool| tool["name"] == "mw_run")
            .expect("mw_run tool");

        let description = run_tool["description"].as_str().unwrap();
        let lower = description.to_ascii_lowercase();
        assert!(
            lower.contains("presentation-only"),
            "mw_run must advertise presentation-only status: {description}"
        );
        assert!(
            lower.contains("canonical function-entry facts"),
            "mw_run must point entry strings back to Marrow function-entry facts: {description}"
        );
        assert!(
            lower.contains("not a stable production entry api"),
            "mw_run must not present entry strings as a stable production API: {description}"
        );

        let entry_description = run_tool["inputSchema"]["properties"]["entry"]["description"]
            .as_str()
            .unwrap();
        let lower = entry_description.to_ascii_lowercase();
        assert!(
            lower.contains("entry string"),
            "mw_run.entry must describe the entry string: {entry_description}"
        );
        assert!(
            lower.contains("canonical function-entry facts"),
            "mw_run.entry must point entry strings back to Marrow function-entry facts: {entry_description}"
        );
        assert!(
            lower.contains("not a stable production entry api"),
            "mw_run.entry must not present entry strings as a stable production API: {entry_description}"
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
    fn mw_resource_schema_requires_a_named_resource() {
        let policy = Policy { allow_data: false };
        let error = call("mw_resource_schema", &json!({ "file": "/x.mw" }), policy)
            .expect_err("mw_resource_schema must require a resource name");
        assert!(
            error.contains("name"),
            "missing resource name should be a protocol argument error: {error}"
        );
    }

    #[test]
    fn tool_arguments_reject_non_array_run_args() {
        let policy = Policy { allow_data: false };
        let error = call(
            "mw_run",
            &json!({
                "file": "/nope/project/src/main.mw",
                "entry": "app::main",
                "args": 1,
            }),
            policy,
        )
        .expect_err("non-array run args must be a protocol argument error");
        assert!(
            error.contains("args"),
            "mw_run should report the non-array args argument: {error}"
        );
    }

    #[test]
    fn mw_run_blocks_non_empty_args() {
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
            "mw_run args must be blocked before project loading: {result}"
        );
    }

    #[test]
    fn mw_run_blocks_non_empty_test_mode_args() {
        let policy = Policy { allow_data: false };
        let result = call(
            "mw_run",
            &json!({
                "file": "/nope/project/src/main.mw",
                "mode": "test",
                "args": [1],
            }),
            policy,
        )
        .unwrap();
        assert_eq!(
            result["diagnostics"][0]["code"], "mcp.run.args",
            "mw_run test args must be blocked before project loading: {result}"
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
