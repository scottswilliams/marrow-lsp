//! The MCP request dispatcher: the tool catalog and the `tools/call` routing.
//!
//! Every handler is one call into [`marrow_lsp_core::mcp`] plus argument parsing —
//! no analysis lives here. The catalog ([`tools`]) is the JSON Schema the client
//! shows an agent; [`call`] decodes a `tools/call`'s arguments and returns the
//! tool's JSON result, which the transport wraps in the MCP content envelope.

use std::path::PathBuf;

use marrow_lsp_core::data_explorer::DataChildrenRequest;
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

fn data_key_schema(description: &str) -> Json {
    json!({
        "description": description,
        "oneOf": [
            { "type": "object", "properties": { "kind": { "const": "int" }, "value": { "type": "integer" } }, "required": ["kind", "value"] },
            { "type": "object", "properties": { "kind": { "const": "bool" }, "value": { "type": "boolean" } }, "required": ["kind", "value"] },
            { "type": "object", "properties": { "kind": { "const": "string" }, "value": { "type": "string" } }, "required": ["kind", "value"] },
            { "type": "object", "properties": { "kind": { "const": "date" }, "value": { "type": "integer" } }, "required": ["kind", "value"] },
            { "type": "object", "properties": { "kind": { "const": "instant" }, "value": { "type": "string" } }, "required": ["kind", "value"] },
            { "type": "object", "properties": { "kind": { "const": "duration" }, "value": { "type": "string" } }, "required": ["kind", "value"] },
            { "type": "object", "properties": { "kind": { "const": "bytes" }, "value": { "type": "array", "items": { "type": "integer", "minimum": 0, "maximum": 255 } } }, "required": ["kind", "value"] },
        ],
    })
}

fn data_path_segment_schema() -> Json {
    let named_segment = |kind: &str| {
        json!({
            "type": "object",
            "properties": {
                "kind": { "const": kind },
                "value": { "type": "string" },
            },
            "required": ["kind", "value"],
        })
    };
    json!({
        "oneOf": [
            named_segment("root"),
            named_segment("field"),
            named_segment("layer"),
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "key" },
                    "value": data_key_schema("Typed saved-data key segment."),
                },
                "required": ["kind", "value"],
            },
        ],
    })
}

fn production_contract(description: &str) -> Json {
    json!({
        "status": "ready",
        "stableProductionApi": true,
        "description": description,
        "missingFacts": [],
    })
}

fn presentation_contract(description: &str, missing_facts: &[&str]) -> Json {
    json!({
        "status": "presentation-only",
        "stableProductionApi": false,
        "description": description,
        "missingFacts": missing_facts,
    })
}

fn marrow_meta(contract: Json) -> Json {
    json!({ "marrow/contract": contract })
}

/// The tool catalog: one entry per agent tool, each with the JSON Schema for its
/// arguments. This is the smallest set that closes an agent's `.mw` loop — check,
/// inspect a type, complete, read the schema, inspect saved data (gated), and run.
pub fn tools() -> Json {
    json!([
        {
            "name": "mw_check",
            "description": "Check Marrow source and return diagnostics. Pass `file` (a path inside a project) to type-check the whole project — optionally with `source` to overlay an unsaved edit — or pass a bare `source` snippet for syntax-only checking. This is the primary compile-error feedback tool.",
            "_meta": marrow_meta(production_contract("compile feedback")),
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
            "_meta": marrow_meta(json!({
                "status": "ready",
                "stableProductionApi": true,
                "description": "type inspection",
                "basis": "Marrow checked type_at facts",
                "missingFacts": [],
            })),
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
            "_meta": marrow_meta(presentation_contract(
                "development helper",
                &["canonical completion-context facts"],
            )),
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
            "_meta": marrow_meta(json!({
                "status": "presentation-only",
                "stableProductionApi": false,
                "description": "development inspection helper",
                "basis": "current checked schema facts",
                "missingFacts": [
                    "catalog-bound resource/store/member identity",
                    "presence/default facts",
                    "typed protocol DTOs",
                ],
                "boundedness": {
                    "requiresNamedResource": true,
                    "wholeCatalog": "blocked",
                    "blocker": "paged catalog/schema DTOs",
                },
            })),
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
            "description": "Presentation-only saved-root listing helper: list saved root names and Marrow store_snapshot metadata from the project's real store when data access is enabled. It returns no child paths or stored values, accepts no editor-authored saved path, and reads nothing when data access is disabled. Missing catalog-bound saved-place identity and stable saved-data DTOs; not a stable typed production API.",
            "_meta": marrow_meta(json!({
                "status": "presentation-only",
                "stableProductionApi": false,
                "description": "saved-root listing helper",
                "missingFacts": [
                    "catalog-bound saved-place identity",
                    "stable saved-data DTOs",
                ],
                "dataAccess": "gated",
                "pathSurface": {
                    "rootOnly": true,
                    "childPaths": "absent",
                    "editorAuthoredSavedPath": "blocked",
                },
            })),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                },
                "required": ["file"],
            },
        },
        {
            "name": "mw_data_children",
            "description": "Presentation-only bounded typed data helper: return one page of typed child segments and Marrow store_snapshot metadata under a saved-data path from the project's real store when data access is enabled. It accepts a typed saved-data path and an optional typed cursor DTO, clamps `limit`, and reads nothing when data access is disabled. Missing catalog-bound saved-place identity; not a stable production data API.",
            "_meta": marrow_meta(json!({
                "status": "presentation-only",
                "stableProductionApi": false,
                "description": "bounded typed data helper",
                "missingFacts": [
                    "catalog-bound saved-place identity",
                ],
                "dataAccess": "gated",
                "basis": "Marrow typed data children tooling",
                "boundedness": {
                    "page": "limit-clamped",
                    "unboundedWalk": false,
                },
                "pathSurface": {
                    "segments": "typed-saved-data-path",
                    "cursor": "typed",
                    "memberExpansion": "Marrow-owned",
                    "savedPathString": "absent",
                },
            })),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "segments": {
                        "type": "array",
                        "description": "Typed saved-data path segments, for example [{\"kind\":\"root\",\"value\":\"books\"},{\"kind\":\"key\",\"value\":{\"kind\":\"int\",\"value\":1}}].",
                        "minItems": 1,
                        "items": data_path_segment_schema(),
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum first identity keys to return; the core clamps oversized requests.",
                    },
                    "cursor": data_key_schema("Optional typed cursor returned by the previous page."),
                },
                "required": ["file", "segments", "limit"],
            },
        },
        {
            "name": "mw_data_integrity",
            "description": "Debug/admin advisory over the current schema and real store: scan the project's real stored data and report capped/current-schema advisory findings such as orphan paths (roots or members the schema no longer declares) or values that no longer decode as their declared type. This is the capped, on-demand schema-change-impact advisory — the same check `marrow data integrity` runs — not complete production validation. Gated behind data access; returns a refusal envelope and reads nothing when disabled. This is not catalog-epoch/store-generation-bound production validation or repair.",
            "_meta": marrow_meta(json!({
                "status": "presentation-only",
                "stableProductionApi": false,
                "description": "debug/admin advisory",
                "basis": "current schema and real store",
                "missingFacts": [
                    "catalog/store identity",
                    "store generation",
                    "catalog epoch/digest",
                    "typed repair or drift facts",
                ],
                "dataAccess": "gated",
                "scope": "capped-current-schema-advisory",
                "advisory": true,
                "debugAdmin": true,
                "productionValidation": false,
                "repair": false,
            })),
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
            "_meta": marrow_meta(json!({
                "status": "presentation-only",
                "stableProductionApi": false,
                "description": "sandboxed execution helper",
                "missingFacts": [
                    "canonical function-entry facts",
                    "transitive effect facts",
                    "durable-scope facts",
                    "transaction facts",
                    "runtime generation facts",
                    "typed run protocol DTOs",
                ],
                "sandbox": "fresh-in-memory-store",
                "host": "locked-down",
                "realStore": "never-touched",
                "catalogState": "not-established",
                "blockedInputs": {
                    "args": {
                        "status": "blocked",
                        "blocker": "typed run argument facts",
                        "missingFacts": ["typed run argument facts"],
                        "appliesToModes": ["run", "test"],
                    },
                },
                "presentationInputs": {
                    "entry": {
                        "status": "presentation-only",
                        "blocker": "canonical function-entry facts",
                        "missingFacts": ["canonical function-entry facts"],
                        "stableProductionApi": false,
                    },
                },
            })),
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
            let file = optional_path(arguments, "file")?;
            let source = optional_str(arguments, "source")?;
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
        "mw_data_children" => {
            let file = required_path(arguments, "file")?;
            let request = data_children_request(arguments)?;
            Ok(mcp::data_children(&file, request, policy.allow_data))
        }
        "mw_data_integrity" => {
            let file = required_path(arguments, "file")?;
            Ok(mcp::data_integrity(&file, policy.allow_data))
        }
        "mw_run" => {
            let file = required_path(arguments, "file")?;
            let entry = optional_str(arguments, "entry")?;
            let args = optional_array(arguments, "args")?;
            let mode = match optional_str(arguments, "mode")? {
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

/// An optional string argument. `null` is absent; other concrete values must be strings.
fn optional_str<'a>(arguments: &'a Json, key: &str) -> Result<Option<&'a str>, String> {
    match arguments.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(value)) => Ok(Some(value)),
        Some(_) => Err(format!("argument `{key}` must be a string when present")),
    }
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

fn data_children_request(arguments: &Json) -> Result<DataChildrenRequest, String> {
    serde_json::from_value(arguments.clone())
        .map_err(|error| format!("invalid mw_data_children arguments: {error}"))
}

/// A required path argument as a `PathBuf`.
fn required_path(arguments: &Json, key: &str) -> Result<PathBuf, String> {
    required_str(arguments, key).map(PathBuf::from)
}

/// An optional path argument as a `PathBuf`.
fn optional_path(arguments: &Json, key: &str) -> Result<Option<PathBuf>, String> {
    Ok(optional_str(arguments, key)?.map(PathBuf::from))
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
            "mw_data_children",
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

    fn tool<'a>(tools: &'a [Json], name: &str) -> &'a Json {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("missing MCP tool {name}"))
    }

    fn contract<'a>(tools: &'a [Json], name: &str) -> &'a Json {
        tool(tools, name)
            .get("_meta")
            .and_then(|meta| meta.get("marrow/contract"))
            .unwrap_or_else(|| panic!("{name} must expose _meta[\"marrow/contract\"]"))
    }

    fn strings(value: &Json) -> Vec<String> {
        value
            .as_array()
            .expect("array")
            .iter()
            .map(|item| item.as_str().expect("string").to_string())
            .collect::<Vec<_>>()
    }

    #[test]
    fn catalog_exposes_production_tool_contracts() {
        let catalog = tools();
        let tools = catalog.as_array().unwrap();

        let check = contract(tools, "mw_check");
        assert_eq!(check["status"], "ready");
        assert_eq!(check["stableProductionApi"], true);
        assert_eq!(check["description"], "compile feedback");
        assert_eq!(strings(&check["missingFacts"]), Vec::<String>::new());

        let type_at = contract(tools, "mw_type_at");
        assert_eq!(type_at["status"], "ready");
        assert_eq!(type_at["stableProductionApi"], true);
        assert_eq!(type_at["description"], "type inspection");
        assert_eq!(type_at["basis"], "Marrow checked type_at facts");
        assert_eq!(strings(&type_at["missingFacts"]), Vec::<String>::new());
    }

    #[test]
    fn catalog_exposes_presentation_tool_contracts() {
        let catalog = tools();
        let tools = catalog.as_array().unwrap();

        let complete = contract(tools, "mw_complete");
        assert_eq!(complete["status"], "presentation-only");
        assert_eq!(complete["stableProductionApi"], false);
        assert_eq!(complete["description"], "development helper");
        assert_eq!(
            strings(&complete["missingFacts"]),
            vec!["canonical completion-context facts"]
        );

        let schema = contract(tools, "mw_resource_schema");
        assert_eq!(schema["status"], "presentation-only");
        assert_eq!(schema["stableProductionApi"], false);
        assert_eq!(schema["description"], "development inspection helper");
        assert_eq!(schema["basis"], "current checked schema facts");
        assert_eq!(
            strings(&schema["missingFacts"]),
            vec![
                "catalog-bound resource/store/member identity",
                "presence/default facts",
                "typed protocol DTOs"
            ]
        );
        assert_eq!(schema["boundedness"]["requiresNamedResource"], true);
        assert_eq!(schema["boundedness"]["wholeCatalog"], "blocked");
        assert_eq!(
            schema["boundedness"]["blocker"],
            "paged catalog/schema DTOs"
        );

        let saved_roots = contract(tools, "mw_saved_roots");
        assert_eq!(saved_roots["status"], "presentation-only");
        assert_eq!(saved_roots["stableProductionApi"], false);
        assert_eq!(saved_roots["description"], "saved-root listing helper");
        assert_eq!(
            strings(&saved_roots["missingFacts"]),
            vec![
                "catalog-bound saved-place identity",
                "stable saved-data DTOs"
            ]
        );
        assert_eq!(saved_roots["dataAccess"], "gated");
        assert_eq!(saved_roots["pathSurface"]["rootOnly"], true);
        assert_eq!(saved_roots["pathSurface"]["childPaths"], "absent");
        assert_eq!(
            saved_roots["pathSurface"]["editorAuthoredSavedPath"],
            "blocked"
        );

        let data_children = contract(tools, "mw_data_children");
        assert_eq!(data_children["status"], "presentation-only");
        assert_eq!(data_children["stableProductionApi"], false);
        assert_eq!(data_children["description"], "bounded typed data helper");
        assert_eq!(
            strings(&data_children["missingFacts"]),
            vec!["catalog-bound saved-place identity"]
        );
        assert_eq!(data_children["dataAccess"], "gated");
        assert_eq!(data_children["boundedness"]["page"], "limit-clamped");
        assert_eq!(
            data_children["pathSurface"]["segments"],
            "typed-saved-data-path"
        );
        assert_eq!(data_children["pathSurface"]["cursor"], "typed");
        assert_eq!(
            data_children["pathSurface"]["memberExpansion"],
            "Marrow-owned"
        );

        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.iter().all(|name| *name != "mw_saved_get"));
        assert!(
            names.iter().all(|name| *name != "mw_saved_children"),
            "saved-data tools that accept client-authored paths stay absent"
        );

        assert_eq!(
            tool(tools, "mw_resource_schema")["inputSchema"]["required"],
            json!(["file", "name"])
        );
        assert_eq!(
            tool(tools, "mw_data_children")["inputSchema"]["required"],
            json!(["file", "segments", "limit"])
        );
        let segments = &tool(tools, "mw_data_children")["inputSchema"]["properties"]["segments"];
        assert_eq!(segments["minItems"], 1);
        assert_eq!(segments.get("maxItems"), None);
        let segment_kinds = segments["items"]["oneOf"]
            .as_array()
            .expect("segment alternatives")
            .iter()
            .map(|schema| schema["properties"]["kind"]["const"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(segment_kinds, vec!["root", "field", "layer", "key"]);
    }

    #[test]
    fn catalog_exposes_data_integrity_advisory_contract() {
        let catalog = tools();
        let tools = catalog.as_array().unwrap();

        let integrity = contract(tools, "mw_data_integrity");
        assert_eq!(integrity["status"], "presentation-only");
        assert_eq!(integrity["stableProductionApi"], false);
        assert_eq!(integrity["description"], "debug/admin advisory");
        assert_eq!(integrity["basis"], "current schema and real store");
        assert_eq!(
            strings(&integrity["missingFacts"]),
            vec![
                "catalog/store identity",
                "store generation",
                "catalog epoch/digest",
                "typed repair or drift facts"
            ]
        );
        assert_eq!(integrity["dataAccess"], "gated");
        assert_eq!(integrity["scope"], "capped-current-schema-advisory");
        assert_eq!(integrity["advisory"], true);
        assert_eq!(integrity["debugAdmin"], true);
        assert_eq!(integrity["productionValidation"], false);
        assert_eq!(integrity["repair"], false);
    }

    #[test]
    fn catalog_exposes_run_contracts() {
        let catalog = tools();
        let tools = catalog.as_array().unwrap();

        let run = contract(tools, "mw_run");
        assert_eq!(run["status"], "presentation-only");
        assert_eq!(run["stableProductionApi"], false);
        assert_eq!(run["description"], "sandboxed execution helper");
        assert_eq!(
            strings(&run["missingFacts"]),
            vec![
                "canonical function-entry facts",
                "transitive effect facts",
                "durable-scope facts",
                "transaction facts",
                "runtime generation facts",
                "typed run protocol DTOs"
            ]
        );
        assert_eq!(run["sandbox"], "fresh-in-memory-store");
        assert_eq!(run["host"], "locked-down");
        assert_eq!(run["realStore"], "never-touched");
        assert_eq!(run["catalogState"], "not-established");
        assert_eq!(run["blockedInputs"]["args"]["status"], "blocked");
        assert_eq!(
            run["blockedInputs"]["args"]["blocker"],
            "typed run argument facts"
        );
        assert_eq!(
            strings(&run["blockedInputs"]["args"]["missingFacts"]),
            vec!["typed run argument facts"]
        );
        assert_eq!(
            run["blockedInputs"]["args"]["appliesToModes"],
            json!(["run", "test"])
        );
        assert_eq!(
            run["presentationInputs"]["entry"]["status"],
            "presentation-only"
        );
        assert_eq!(
            run["presentationInputs"]["entry"]["blocker"],
            "canonical function-entry facts"
        );
        assert_eq!(
            strings(&run["presentationInputs"]["entry"]["missingFacts"]),
            vec!["canonical function-entry facts"]
        );
        assert_eq!(
            run["presentationInputs"]["entry"]["stableProductionApi"],
            false
        );

        let removed_args_opt_in = ["allow", "Prototype", "Args"].concat();
        assert!(
            tool(tools, "mw_run")["inputSchema"]["properties"]
                .get(&removed_args_opt_in)
                .is_none(),
            "mw_run schema must not expose an argument opt-in"
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
        let result = call(
            "mw_data_children",
            &json!({
                "file": "/nope/x.mw",
                "segments": [{ "kind": "root", "value": "counter" }],
                "limit": 1,
            }),
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
    fn mw_data_children_rejects_invalid_typed_arguments() {
        let policy = Policy { allow_data: true };
        let error = call(
            "mw_data_children",
            &json!({
                "file": "/nope/project/src/main.mw",
                "segments": "counter",
                "limit": 1,
            }),
            policy,
        )
        .expect_err("mw_data_children must deserialize typed request arguments");
        assert!(
            error.contains("mw_data_children"),
            "invalid data children args should name the tool: {error}"
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
    fn optional_string_arguments_reject_concrete_non_strings() {
        let policy = Policy { allow_data: false };
        let cases = [
            (
                "mw_check",
                json!({ "file": 7, "source": "module a" }),
                "file",
            ),
            (
                "mw_check",
                json!({ "file": "/x.mw", "source": 7 }),
                "source",
            ),
            ("mw_run", json!({ "file": "/x.mw", "entry": 7 }), "entry"),
            ("mw_run", json!({ "file": "/x.mw", "mode": 7 }), "mode"),
        ];

        for (tool, arguments, key) in cases {
            let error = call(tool, &arguments, policy)
                .expect_err(&format!("{tool} should reject non-string `{key}`"));
            assert!(
                error.contains(key),
                "{tool} should report the malformed `{key}` argument: {error}"
            );
        }
    }

    #[test]
    fn optional_string_nulls_remain_absent() {
        let policy = Policy { allow_data: false };

        for arguments in [
            json!({ "file": "/x.mw", "mode": null, "args": [1] }),
            json!({ "file": "/x.mw", "entry": null, "args": [1] }),
        ] {
            let result = call("mw_run", &arguments, policy).unwrap();
            assert_eq!(
                result["diagnostics"][0]["code"], "mcp.run.args",
                "null optional strings should be absent so run args still block before project loading: {result}"
            );
        }

        let error = call("mw_check", &json!({ "source": null }), policy)
            .expect_err("null source should be absent, not a source snippet");
        assert!(
            error.contains("file") && error.contains("source"),
            "null source should leave mw_check with neither input: {error}"
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
        assert_eq!(
            result["diagnostics"][0]["code"], "mcp.run.args",
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
