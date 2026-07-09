//! The MCP request dispatcher: the tool catalog and the `tools/call` routing.
//!
//! Every handler is one call into [`marrow_lsp_core::mcp`] plus argument parsing —
//! no analysis lives here. The catalog ([`tools`]) is the JSON Schema the client
//! shows an agent; [`call`] decodes a `tools/call`'s arguments and returns the
//! tool's JSON result, which the transport wraps in the MCP content envelope.

use std::path::PathBuf;
use std::time::Duration;

use marrow_lsp_core::data_explorer::{
    DataChildrenRequest, DataReadRequest, DataRequestValidationError, data_key_input_schema,
    data_path_segment_input_schema, validate_data_children_request, validate_data_read_request,
};
use marrow_lsp_core::mcp::{self, RunMode, SurfaceRouteScope};
use serde_json::{Value as Json, json};

/// The latest MCP protocol version this server speaks, and its default when a
/// client requests none or an unsupported one.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The MCP protocol versions this server accepts, newest first. The tool surface
/// (`initialize` / `tools/list` / `tools/call`) is stable across this range, and a
/// newer field such as `structuredContent` is additive, so an older client simply
/// ignores it.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[PROTOCOL_VERSION, "2025-03-26"];

/// Negotiate the protocol version to answer `initialize` with: echo the client's
/// requested version when this server supports it, otherwise answer with the
/// latest supported version and let the client decide whether to proceed. A client
/// that names no version gets the latest.
pub fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    match requested {
        Some(requested) => SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .find(|version| **version == requested)
            .copied()
            .unwrap_or(PROTOCOL_VERSION),
        None => PROTOCOL_VERSION,
    }
}

/// The server's runtime policy: whether the data tools may read real stored data,
/// whether they may write it (a separate opt-in from read), how long a single
/// `mw_run` may execute, and the directory tree tools are confined to. Set once at
/// launch from the environment / flags and never mutated, so a tool can never
/// escalate its own access, budget, or reach mid-session.
///
/// Read and write are separate grants rather than one "data access" bit: the read
/// tools receive a [`ReadAccess`](mcp::ReadAccess) and `mw_surface_write` receives a
/// distinct [`WriteAccess`](mcp::WriteAccess), so a read-only policy yields no write
/// grant and the writable executor is unreachable — not merely gated at runtime.
///
/// The allowed root is not held here: it is applied to the core once at launch (see
/// [`crate::main`]) and the core is its sole owner, so the confinement authority is
/// never represented twice.
#[derive(Clone)]
pub struct Policy {
    pub allow_read: bool,
    pub allow_write: bool,
    pub run_budget: Duration,
}

impl Policy {
    pub fn new(allow_read: bool, allow_write: bool, run_budget: Duration) -> Self {
        Self {
            allow_read,
            allow_write,
            run_budget,
        }
    }

    /// The read grant to hand a data read, present only when read access is on.
    fn read_access(&self) -> Option<mcp::ReadAccess> {
        self.allow_read.then(mcp::ReadAccess::granted)
    }

    /// The write grant to hand `mw_surface_write`, present only when the write
    /// opt-in is on. A read-only policy returns `None` here, so the write path is
    /// never entered — the read grant is a different type and cannot substitute.
    fn write_access(&self) -> Option<mcp::WriteAccess> {
        self.allow_write.then(mcp::WriteAccess::granted)
    }
}

/// The `serverInfo` block of the `initialize` reply.
pub fn server_info() -> Json {
    json!({ "name": "marrow-mcp", "version": env!("CARGO_PKG_VERSION") })
}

/// The result of an `initialize` request: the negotiated protocol version, the
/// server's capabilities (it serves tools), and its identity. Capabilities
/// advertise only tools — this server exposes no resources or prompts.
pub fn initialize_result(protocol_version: &str) -> Json {
    json!({
        "protocolVersion": protocol_version,
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

fn namespace_qualifier_schema() -> Json {
    json!({
        "type": "array",
        "description": "One or more namespace/name segments such as [\"shelf\", \"books\"] or [\"books\"] for an imported module alias.",
        "items": { "type": "string", "minLength": 1 },
        "minItems": 1,
    })
}

fn run_input_schema() -> Json {
    let mut run_arg = marrow_run::entry_argument_json_schema();
    let defs = run_arg
        .as_object_mut()
        .and_then(|schema| schema.remove("$defs"))
        .expect("Marrow entry argument schema carries recursive definitions");
    json!({
        "type": "object",
        "properties": {
            "file": string_prop("Absolute path to any .mw file inside the project."),
            "entry": string_prop("Entry selector such as `module::fn` for run mode."),
            "args": {
                "type": "array",
                "items": run_arg,
                "description": "Typed named arguments for run mode. Test mode does not accept args.",
            },
            "mode": {
                "type": "string",
                "enum": ["run", "test"],
                "description": "`run` (default) executes `entry`; `test` runs the project's tests.",
            },
        },
        "required": ["file"],
        "$defs": defs,
    })
}

fn surface_operation_schema() -> Json {
    json!({
        "type": "object",
        "description": "Canonical marrow_json surface.operation.v1 request envelope. The body is decoded only by Marrow's SurfaceOperationRequestJson DTO and executed by Marrow's surface executor.",
        "properties": {
            "profile_version": { "const": "surface.operation.v1" },
            "operation_tag": string_prop("Stable operation tag from mw_surface_routes."),
            "request": {
                "type": "object",
                "description": "Canonical surface operation request body.",
            },
        },
        "required": ["profile_version", "operation_tag", "request"],
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

fn namespace_completion_contract() -> Json {
    let mut contract = production_contract("source namespace completion fact");
    contract["basis"] = json!("Marrow source namespace completion facts");
    contract
}

fn completion_contract() -> Json {
    let mut contract = production_contract("source completion fact");
    contract["basis"] = json!("Marrow source completion fact");
    contract
}

fn marrow_meta(contract: Json) -> Json {
    json!({ "marrow/contract": contract, "marrow/boundary": boundary_statement() })
}

/// What a data-enabled server can and cannot reach, stated plainly so an operator
/// reading a tool's metadata knows the trust boundary before granting access. It is
/// a description of the server's guarantees, not a per-tool contract, so every tool
/// carries the same statement.
fn boundary_statement() -> Json {
    json!({
        "reach": "data, source, and run tools operate only on projects whose resolved root is under the operator's allowed root; a file outside it is refused before any store is opened",
        "runSandbox": "mw_run executes project source but only against a fresh in-memory store — the project's real store is never opened",
        "writeGrant": "the write grant is as strong as the project's most destructive surface action; it is not scoped to individual records",
        "auditTrail": "the write-audit trail is an operational record of what an agent changed, not a tamper-evident log",
        "grants": "read access, write access, and the allowed root come only from launch flags or environment variables, never from a tool call",
    })
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
            "description": "Return Marrow's source.completion.v1 fact at a position in `file`, including source completion items with stable semantic kinds, labels, detail text, and docs.",
            "_meta": marrow_meta(completion_contract()),
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
            "name": "mw_namespace_complete",
            "description": "Return Marrow's source.namespace.completion.v1 fact for a concrete project module or enum qualifier. This production tool accepts only a file and namespace/name segments; broad editor completion remains mw_complete.",
            "_meta": marrow_meta(namespace_completion_contract()),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "qualifier": namespace_qualifier_schema(),
                },
                "required": ["file", "qualifier"],
            },
        },
        {
            "name": "mw_resource_schema",
            "description": "Return Marrow's canonical resource.schema.v1 JSON DTO for one named resource schema, including catalog-bound resource, store, index, and member identities. The resource `name` is required; all-resource listing is outside this named lookup surface.",
            "_meta": marrow_meta(json!({
                "status": "ready",
                "stableProductionApi": true,
                "description": "resource schema DTO",
                "basis": "Marrow resource schema DTO",
                "missingFacts": [],
                "boundedness": {
                    "requiresNamedResource": true,
                    "wholeCatalog": "outside-surface",
                },
            })),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "name": string_prop("Resource name for the canonical resource.schema.v1 named lookup surface."),
                },
                "required": ["file", "name"],
            },
        },
        {
            "name": "mw_surface_routes",
            "description": "Return the Marrow-owned surface route manifest for the checked project using the canonical surface ABI and route DTOs. By default this returns read routes only; pass includeWrites true to explicitly include update/action routes. This opens no data store and needs no data-access opt-in.",
            "_meta": marrow_meta(json!({
                "status": "ready",
                "stableProductionApi": true,
                "description": "surface route manifest",
                "basis": "Marrow surface route manifest DTO",
                "missingFacts": [],
                "dataAccess": "not-required",
                "operations": "read/update/action by explicit route scope",
            })),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "includeWrites": {
                        "type": "boolean",
                        "description": "When true, include canonical update/action routes as well as read routes. Defaults to false.",
                    },
                },
                "required": ["file"],
            },
        },
        {
            "name": "mw_surface_read",
            "description": "Execute one canonical surface.operation.v1 read request against the project's Marrow surface read-only session. This is gated by data access, refuses before project/store opening when disabled, and returns canonical Marrow surface operation response or error envelopes.",
            "_meta": marrow_meta(json!({
                "status": "ready",
                "stableProductionApi": true,
                "description": "surface read operation",
                "basis": "Marrow surface operation read-only executor",
                "missingFacts": [],
                "dataAccess": "gated",
                "operations": "read-only",
            })),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "operation": surface_operation_schema(),
                },
                "required": ["file", "operation"],
            },
        },
        {
            "name": "mw_surface_write",
            "description": "Execute one canonical surface.operation.v1 read, update, or action request against the project's writable Marrow surface session. This is gated by data access, refuses before project/store opening when disabled, and returns canonical Marrow surface operation response or error envelopes.",
            "_meta": marrow_meta(json!({
                "status": "ready",
                "stableProductionApi": true,
                "description": "surface write operation",
                "basis": "Marrow surface operation writable executor",
                "missingFacts": [],
                "dataAccess": "gated",
                "writeOptIn": "gated",
                "operations": "read/update/action",
            })),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "operation": surface_operation_schema(),
                },
                "required": ["file", "operation"],
            },
        },
        {
            "name": "mw_saved_roots",
            "description": "Production read-only saved-root listing API: list typed saved-root views with Marrow data_view_boundary facts and generation/snapshot metadata from the project's real store when data access is enabled. It returns no child paths or stored values, accepts no editor-authored saved path, and reads nothing when data access is disabled.",
            "_meta": marrow_meta(mcp::saved_roots_contract()),
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
            "description": "Production read-only bounded typed data children API: return one page of typed child segments with Marrow data_view_boundary facts and generation/snapshot metadata for a saved-data path from the project's real store when data access is enabled. It accepts a catalog-bound typed saved-data path and an optional typed cursor DTO, clamps `limit`, and reads nothing when data access is disabled.",
            "_meta": marrow_meta(mcp::data_children_contract()),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "segments": {
                        "type": "array",
                        "description": "Catalog-bound typed saved-data path segments, for example [{\"kind\":\"root\",\"store_catalog_id\":\"cat_00000000000000000000000000000001\"},{\"kind\":\"key\",\"value\":{\"kind\":\"int\",\"value\":1}}].",
                        "minItems": 1,
                        "items": data_path_segment_input_schema(),
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum first identity keys to return; the core clamps oversized requests.",
                    },
                    "cursor": data_key_input_schema("Optional typed cursor returned by the previous page."),
                },
                "required": ["file", "segments", "limit"],
            },
        },
        {
            "name": "mw_data_read",
            "description": "Production read-only bounded typed data read API: return a Marrow-rendered value preview with Marrow data_view_boundary facts and generation/snapshot metadata for one typed saved-data path from the project's real store when data access is enabled. It accepts a catalog-bound typed saved-data path, clamps `preview_limit`, uses Marrow-owned bounded read presence, and reads nothing when data access is disabled.",
            "_meta": marrow_meta(mcp::data_read_contract()),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": string_prop("Absolute path to any .mw file inside the project."),
                    "segments": {
                        "type": "array",
                        "description": "Catalog-bound typed saved-data path segments, for example [{\"kind\":\"root\",\"store_catalog_id\":\"cat_00000000000000000000000000000001\"},{\"kind\":\"key\",\"value\":{\"kind\":\"int\",\"value\":1}},{\"kind\":\"field\",\"member_catalog_id\":\"cat_00000000000000000000000000000002\"}].",
                        "minItems": 1,
                        "items": data_path_segment_input_schema(),
                    },
                    "preview_limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum rendered preview characters; the core clamps oversized requests.",
                    },
                },
                "required": ["file", "segments"],
            },
        },
        {
            "name": "mw_run",
            "description": "Sandboxed execution API: execute Marrow to confirm behavior through Marrow's fresh in-memory project session and execution boundary under a locked-down host (fixed clock + captured log, no filesystem/env/maintenance) — the project's real store is never touched. `mode: \"run\"` resolves `entry` against Marrow's current entry descriptor, accepts typed `args`, and returns Marrow's run result/error DTO plus execution boundary with source analysis generation, entry descriptor, footprint, cost-shape, and store-open-mode facts. `mode: \"test\"` runs the project's test session, returns Marrow's test session execution boundary with source analysis generation, runs each test over its own fresh store, and does not accept args.",
            "_meta": marrow_meta(json!({
                "status": "ready",
                "stableProductionApi": true,
                "description": "sandboxed execution API",
                "missingFacts": [],
                "sandbox": "fresh-in-memory-store",
                "host": "locked-down",
                "realStore": "never-touched",
                "catalogState": "not-mutated",
                "runResult": "Marrow run result/error DTOs",
                "runFacts": "Marrow execution boundary with source analysis generation, entry descriptor, footprint, cost shape, and store open mode",
                "testBoundary": "Marrow test session execution boundary",
                "typedInputs": {
                    "args": "typed run protocol DTOs",
                    "entry": "current-session entry selector",
                },
            })),
            "inputSchema": run_input_schema(),
        },
    ])
}

/// Route a `tools/call` to its core handler, returning the tool's JSON result.
/// `Err` carries a message for an invalid-arguments protocol error (a missing
/// required argument or an unknown tool); `Ok` carries the tool's own result,
/// which the transport renders into the MCP content envelope.
pub fn call(name: &str, arguments: &Json, policy: &Policy) -> Result<Json, String> {
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
        "mw_namespace_complete" => {
            let file = required_path(arguments, "file")?;
            let qualifier = namespace_qualifier(arguments)?;
            Ok(mcp::namespace_complete(&file, &qualifier))
        }
        "mw_resource_schema" => {
            let file = required_path(arguments, "file")?;
            let name = required_str(arguments, "name")?;
            Ok(mcp::resource_schema(&file, name))
        }
        "mw_surface_routes" => {
            let file = required_path(arguments, "file")?;
            let scope = if optional_bool(arguments, "includeWrites")? {
                SurfaceRouteScope::All
            } else {
                SurfaceRouteScope::ReadOnly
            };
            Ok(mcp::surface_routes(&file, scope))
        }
        "mw_surface_read" => {
            let file = required_path(arguments, "file")?;
            let operation = required_object(arguments, "operation")?;
            mcp::surface_read(&file, operation, policy.read_access())
        }
        "mw_surface_write" => {
            let file = required_path(arguments, "file")?;
            let operation = required_object(arguments, "operation")?;
            mcp::surface_write(&file, operation, policy.write_access())
        }
        "mw_saved_roots" => {
            let file = required_path(arguments, "file")?;
            Ok(mcp::saved_roots(&file, policy.read_access()))
        }
        "mw_data_children" => {
            let file = required_path(arguments, "file")?;
            let request = data_children_request(arguments)?;
            Ok(mcp::data_children(&file, request, policy.read_access()))
        }
        "mw_data_read" => {
            let file = required_path(arguments, "file")?;
            let request = data_read_request(arguments)?;
            Ok(mcp::data_read(&file, request, policy.read_access()))
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
            Ok(mcp::run(&file, entry, &args, mode, policy.run_budget))
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

fn optional_bool(arguments: &Json, key: &str) -> Result<bool, String> {
    match arguments.get(key) {
        None | Some(Json::Null) => Ok(false),
        Some(Json::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("argument `{key}` must be a boolean when present")),
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

fn namespace_qualifier(arguments: &Json) -> Result<Vec<String>, String> {
    let invalid = || {
        "invalid mw_namespace_complete arguments: `qualifier` must be a non-empty array of non-empty string segments".to_string()
    };
    let Some(Json::Array(values)) = arguments.get("qualifier") else {
        return Err(invalid());
    };
    if values.is_empty() {
        return Err(invalid());
    }
    values
        .iter()
        .map(|value| match value.as_str() {
            Some(segment) if !segment.is_empty() && !segment.contains("::") => {
                Ok(segment.to_string())
            }
            _ => Err(invalid()),
        })
        .collect()
}

fn required_object(arguments: &Json, key: &str) -> Result<Json, String> {
    match arguments.get(key) {
        Some(value @ Json::Object(_)) => Ok(value.clone()),
        _ => Err(format!("missing or non-object argument `{key}`")),
    }
}

fn data_children_request(arguments: &Json) -> Result<DataChildrenRequest, String> {
    let request: DataChildrenRequest = serde_json::from_value(arguments.clone())
        .map_err(|error| format!("invalid mw_data_children arguments: {error}"))?;
    validate_data_children_request(request)
        .map_err(|error| invalid_data_request("mw_data_children", error))
}

fn data_read_request(arguments: &Json) -> Result<DataReadRequest, String> {
    let request: DataReadRequest = serde_json::from_value(arguments.clone())
        .map_err(|error| format!("invalid mw_data_read arguments: {error}"))?;
    validate_data_read_request(request).map_err(|error| invalid_data_request("mw_data_read", error))
}

fn invalid_data_request(tool: &str, error: DataRequestValidationError) -> String {
    format!("invalid {tool} arguments: {}", error.message())
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
            "mw_namespace_complete",
            "mw_resource_schema",
            "mw_surface_routes",
            "mw_surface_read",
            "mw_surface_write",
            "mw_saved_roots",
            "mw_data_children",
            "mw_data_read",
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

        let complete = contract(tools, "mw_complete");
        assert_eq!(complete["status"], "ready");
        assert_eq!(complete["stableProductionApi"], true);
        assert_eq!(complete["description"], "source completion fact");
        assert_eq!(complete["basis"], "Marrow source completion fact");
        assert_eq!(strings(&complete["missingFacts"]), Vec::<String>::new());

        let schema = contract(tools, "mw_resource_schema");
        assert_eq!(schema["status"], "ready");
        assert_eq!(schema["stableProductionApi"], true);
        assert_eq!(schema["description"], "resource schema DTO");
        assert_eq!(schema["basis"], "Marrow resource schema DTO");
        assert_eq!(strings(&schema["missingFacts"]), Vec::<String>::new());
        assert_eq!(schema["boundedness"]["requiresNamedResource"], true);
        assert_eq!(schema["boundedness"]["wholeCatalog"], "outside-surface");
        assert_eq!(
            tool(tools, "mw_resource_schema")["inputSchema"]["properties"]["name"]["description"],
            "Resource name for the canonical resource.schema.v1 named lookup surface."
        );
        assert_eq!(
            tool(tools, "mw_resource_schema")["inputSchema"]["required"],
            json!(["file", "name"])
        );

        let namespace_complete = contract(tools, "mw_namespace_complete");
        assert_eq!(namespace_complete["status"], "ready");
        assert_eq!(namespace_complete["stableProductionApi"], true);
        assert_eq!(
            namespace_complete["description"],
            "source namespace completion fact"
        );
        assert_eq!(
            namespace_complete["basis"],
            "Marrow source namespace completion facts"
        );
        assert_eq!(
            strings(&namespace_complete["missingFacts"]),
            Vec::<String>::new()
        );
        assert_eq!(
            tool(tools, "mw_namespace_complete")["inputSchema"]["required"],
            json!(["file", "qualifier"])
        );
        assert_eq!(
            tool(tools, "mw_namespace_complete")["inputSchema"]["properties"]["qualifier"]["items"]
                ["type"],
            "string"
        );

        let surface_routes = contract(tools, "mw_surface_routes");
        assert_eq!(surface_routes["status"], "ready");
        assert_eq!(surface_routes["stableProductionApi"], true);
        assert_eq!(surface_routes["description"], "surface route manifest");
        assert_eq!(surface_routes["basis"], "Marrow surface route manifest DTO");
        assert_eq!(
            strings(&surface_routes["missingFacts"]),
            Vec::<String>::new()
        );
        assert_eq!(surface_routes["dataAccess"], "not-required");
        assert_eq!(
            surface_routes["operations"],
            "read/update/action by explicit route scope"
        );

        let surface_read = contract(tools, "mw_surface_read");
        assert_eq!(surface_read["status"], "ready");
        assert_eq!(surface_read["stableProductionApi"], true);
        assert_eq!(surface_read["description"], "surface read operation");
        assert_eq!(
            surface_read["basis"],
            "Marrow surface operation read-only executor"
        );
        assert_eq!(strings(&surface_read["missingFacts"]), Vec::<String>::new());
        assert_eq!(surface_read["dataAccess"], "gated");
        assert_eq!(surface_read["operations"], "read-only");

        let surface_write = contract(tools, "mw_surface_write");
        assert_eq!(surface_write["status"], "ready");
        assert_eq!(surface_write["stableProductionApi"], true);
        assert_eq!(surface_write["description"], "surface write operation");
        assert_eq!(
            surface_write["basis"],
            "Marrow surface operation writable executor"
        );
        assert_eq!(
            strings(&surface_write["missingFacts"]),
            Vec::<String>::new()
        );
        assert_eq!(surface_write["dataAccess"], "gated");
        // The write opt-in is declared distinct from read data access, so an agent
        // reading the contract can tell that writing needs its own grant.
        assert_eq!(surface_write["writeOptIn"], "gated");
        assert_eq!(surface_read.get("writeOptIn"), None);
        assert_eq!(surface_write["operations"], "read/update/action");
    }

    #[test]
    fn catalog_exposes_saved_data_read_tool_contracts() {
        let catalog = tools();
        let tools = catalog.as_array().unwrap();

        let saved_roots = contract(tools, "mw_saved_roots");
        assert_eq!(saved_roots["status"], "ready");
        assert_eq!(saved_roots["stableProductionApi"], true);
        assert_eq!(saved_roots["description"], "saved-root listing API");
        assert_eq!(strings(&saved_roots["missingFacts"]), Vec::<String>::new());
        assert_eq!(saved_roots["dataAccess"], "gated");
        assert_eq!(saved_roots["pathSurface"]["rootOnly"], true);
        assert_eq!(saved_roots["pathSurface"]["childPaths"], "absent");
        assert_eq!(
            saved_roots["pathSurface"]["editorAuthoredSavedPath"],
            "blocked"
        );

        let data_children = contract(tools, "mw_data_children");
        assert_eq!(data_children["status"], "ready");
        assert_eq!(data_children["stableProductionApi"], true);
        assert_eq!(
            data_children["description"],
            "bounded typed data children API"
        );
        assert_eq!(
            strings(&data_children["missingFacts"]),
            Vec::<String>::new()
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

        let data_read = contract(tools, "mw_data_read");
        assert_eq!(data_read["status"], "ready");
        assert_eq!(data_read["stableProductionApi"], true);
        assert_eq!(data_read["description"], "bounded typed data read API");
        assert_eq!(strings(&data_read["missingFacts"]), Vec::<String>::new());
        assert_eq!(data_read["dataAccess"], "gated");
        assert_eq!(data_read["basis"], "Marrow typed data read tooling");
        assert_eq!(data_read["boundedness"]["valuePreview"], "limit-clamped");
        assert_eq!(
            data_read["presenceDetection"],
            "Marrow-owned bounded read presence"
        );
        assert_eq!(
            data_read["pathSurface"]["segments"],
            "typed-saved-data-path"
        );
        assert_eq!(data_read["pathSurface"]["savedPathString"], "absent");

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
            tool(tools, "mw_surface_routes")["inputSchema"]["required"],
            json!(["file"])
        );
        assert_eq!(
            tool(tools, "mw_surface_routes")["inputSchema"]["properties"]["includeWrites"]["type"],
            "boolean"
        );
        assert_eq!(
            tool(tools, "mw_surface_read")["inputSchema"]["required"],
            json!(["file", "operation"])
        );
        assert_eq!(
            tool(tools, "mw_surface_write")["inputSchema"]["required"],
            json!(["file", "operation"])
        );
        assert_eq!(
            tool(tools, "mw_data_children")["inputSchema"]["required"],
            json!(["file", "segments", "limit"])
        );
        assert_eq!(
            tool(tools, "mw_data_read")["inputSchema"]["required"],
            json!(["file", "segments"])
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
        let read_segments = &tool(tools, "mw_data_read")["inputSchema"]["properties"]["segments"];
        assert_eq!(read_segments["minItems"], 1);
        assert_eq!(read_segments.get("maxItems"), None);
    }

    #[test]
    fn catalog_saved_data_contracts_match_core_runtime_contracts() {
        let catalog = tools();
        let tools = catalog.as_array().unwrap();

        assert_eq!(
            contract(tools, "mw_saved_roots"),
            &mcp::saved_roots_contract()
        );
        assert_eq!(
            contract(tools, "mw_data_children"),
            &mcp::data_children_contract()
        );
        assert_eq!(contract(tools, "mw_data_read"), &mcp::data_read_contract());
    }

    #[test]
    fn catalog_omits_data_integrity_advisory() {
        let catalog = tools();
        let tools = catalog.as_array().unwrap();

        assert!(
            tools
                .iter()
                .all(|tool| tool["name"].as_str() != Some("mw_data_integrity")),
            "retired advisory data-integrity tool must stay absent from the catalog"
        );
    }

    #[test]
    fn catalog_exposes_run_contracts() {
        let catalog = tools();
        let tools = catalog.as_array().unwrap();

        let run = contract(tools, "mw_run");
        assert_eq!(run["status"], "ready");
        assert_eq!(run["stableProductionApi"], true);
        assert_eq!(run["description"], "sandboxed execution API");
        assert_eq!(run["missingFacts"], json!([]));
        assert_eq!(run["sandbox"], "fresh-in-memory-store");
        assert_eq!(run["host"], "locked-down");
        assert_eq!(run["realStore"], "never-touched");
        assert_eq!(run["catalogState"], "not-mutated");
        assert_eq!(run["runResult"], "Marrow run result/error DTOs");
        assert_eq!(
            run["runFacts"],
            "Marrow execution boundary with source analysis generation, entry descriptor, footprint, cost shape, and store open mode"
        );
        assert_eq!(
            run["testBoundary"],
            "Marrow test session execution boundary"
        );
        assert!(
            tool(tools, "mw_run")["description"]
                .as_str()
                .is_some_and(|description| description.contains("execution boundary")),
            "mw_run description should name the execution boundary"
        );
        assert_eq!(run["typedInputs"]["args"], "typed run protocol DTOs");
        assert_eq!(
            run["typedInputs"]["entry"],
            "current-session entry selector"
        );
        assert!(run.get("blockedInputs").is_none());
        assert!(run.get("presentationInputs").is_none());

        let removed_args_opt_in = ["allow", "Prototype", "Args"].concat();
        assert!(
            tool(tools, "mw_run")["inputSchema"]["properties"]
                .get(&removed_args_opt_in)
                .is_none(),
            "mw_run schema must not expose an argument opt-in"
        );

        let mut expected_arg_schema = marrow_run::entry_argument_json_schema();
        let expected_defs = expected_arg_schema
            .as_object_mut()
            .and_then(|schema| schema.remove("$defs"))
            .expect("Marrow entry argument schema carries definitions");
        let run_schema = &tool(tools, "mw_run")["inputSchema"];
        assert_eq!(run_schema["$defs"], expected_defs);
        assert_eq!(
            run_schema["properties"]["args"]["items"],
            expected_arg_schema
        );
    }

    #[test]
    fn an_unknown_tool_is_an_error() {
        let policy = Policy::new(false, false, mcp::DEFAULT_RUN_BUDGET);
        assert!(call("mw_nope", &json!({}), &policy).is_err());
    }

    #[test]
    fn retired_data_integrity_tool_is_not_callable() {
        let policy = Policy::new(false, false, mcp::DEFAULT_RUN_BUDGET);
        let result = call(
            "mw_data_integrity",
            &json!({ "file": "/nope/x.mw" }),
            &policy,
        );
        assert!(
            result.is_err(),
            "retired advisory data-integrity tool must not remain callable"
        );
    }

    #[test]
    fn a_data_tool_refuses_when_the_policy_disallows_it() {
        let policy = Policy::new(false, false, mcp::DEFAULT_RUN_BUDGET);
        // A nonexistent file is fine: the gate is checked before any project load,
        // so the refusal envelope comes back without touching the filesystem.
        let result = call("mw_saved_roots", &json!({ "file": "/nope/x.mw" }), &policy).unwrap();
        assert_eq!(result["dataAccess"], "disabled");
        let result = call(
            "mw_data_children",
            &json!({
                "file": "/nope/x.mw",
                "segments": [{ "kind": "root", "store_catalog_id": "cat_00000000000000000000000000000001" }],
                "limit": 1,
            }),
            &policy,
        )
        .unwrap();
        assert_eq!(result["dataAccess"], "disabled");
        let result = call(
            "mw_data_read",
            &json!({
                "file": "/nope/x.mw",
                "segments": [{ "kind": "root", "store_catalog_id": "cat_00000000000000000000000000000001" }],
            }),
            &policy,
        )
        .unwrap();
        assert_eq!(result["dataAccess"], "disabled");
        let result = call(
            "mw_surface_read",
            &json!({
                "file": "/nope/x.mw",
                "operation": {
                    "profile_version": "surface.operation.v1",
                    "operation_tag": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                    "request": { "kind": "singleton_read" }
                }
            }),
            &policy,
        )
        .unwrap();
        assert_eq!(result["dataAccess"], "disabled");
        assert_eq!(result["contract"]["status"], "ready");
        assert_eq!(result["contract"]["stableProductionApi"], true);
        let result = call(
            "mw_surface_write",
            &json!({
                "file": "/nope/x.mw",
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
            }),
            &policy,
        )
        .unwrap();
        assert_eq!(result["dataAccess"], "disabled");
        assert_eq!(result["contract"]["status"], "ready");
        assert_eq!(result["contract"]["stableProductionApi"], true);
        assert_eq!(result["contract"]["operations"], "read/update/action");
    }

    #[test]
    fn a_read_only_policy_refuses_a_surface_write() {
        // The trust-critical case the read/write split exists for: reads are opted
        // in, writes are not. `write_access()` yields `None` for a read-only policy,
        // so `mw_surface_write` refuses before opening the store, while a read stays
        // permitted.
        let read_only = Policy::new(true, false, mcp::DEFAULT_RUN_BUDGET);

        let write = call(
            "mw_surface_write",
            &json!({
                "file": "/nope/x.mw",
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
            }),
            &read_only,
        )
        .unwrap();
        assert_eq!(write["dataAccess"], "disabled", "{write}");
        assert_eq!(write["writeOptIn"], "disabled", "{write}");
        assert!(
            write.get("audit").is_none() && write.get("store_stamp").is_none(),
            "a refused write must not open the store or emit an audit record: {write}"
        );

        // The same policy still permits a read: read and write are independent
        // grants. With the read grant present the call gets past the access gate to
        // operation parsing / project loading, so it is never a data-access refusal.
        let read = call(
            "mw_saved_roots",
            &json!({ "file": "/nope/x.mw" }),
            &read_only,
        )
        .unwrap();
        assert_ne!(read["dataAccess"], "disabled", "{read}");
    }

    #[test]
    fn a_missing_required_argument_is_an_error() {
        let policy = Policy::new(false, false, mcp::DEFAULT_RUN_BUDGET);
        assert!(
            call(
                "mw_type_at",
                &json!({ "file": "/x.mw", "line": 0 }),
                &policy
            )
            .is_err()
        );
    }

    #[test]
    fn mw_resource_schema_requires_a_named_resource() {
        let policy = Policy::new(false, false, mcp::DEFAULT_RUN_BUDGET);
        let error = call("mw_resource_schema", &json!({ "file": "/x.mw" }), &policy)
            .expect_err("mw_resource_schema must require a resource name");
        assert!(
            error.contains("name"),
            "missing resource name should be a protocol argument error: {error}"
        );
    }

    #[test]
    fn mw_namespace_complete_rejects_malformed_qualifier_before_project_loading() {
        let policy = Policy::new(false, false, mcp::DEFAULT_RUN_BUDGET);
        for arguments in [
            json!({ "file": "/nope/project/src/main.mw" }),
            json!({ "file": "/nope/project/src/main.mw", "qualifier": null }),
            json!({ "file": "/nope/project/src/main.mw", "qualifier": "books" }),
            json!({ "file": "/nope/project/src/main.mw", "qualifier": [] }),
            json!({ "file": "/nope/project/src/main.mw", "qualifier": ["books", 7] }),
            json!({ "file": "/nope/project/src/main.mw", "qualifier": ["books", ""] }),
        ] {
            let error = call("mw_namespace_complete", &arguments, &policy)
                .expect_err("mw_namespace_complete must validate qualifier arguments");
            assert!(
                error.contains("mw_namespace_complete") && error.contains("qualifier"),
                "invalid namespace completion qualifier should name the tool and argument: {error}"
            );
            assert!(
                !error.contains("marrow.json") && !error.contains("project"),
                "invalid qualifier should fail before project loading: {error}"
            );
        }
    }

    #[test]
    fn mw_data_children_rejects_invalid_typed_arguments() {
        let policy = Policy::new(true, false, mcp::DEFAULT_RUN_BUDGET);
        for arguments in [
            json!({
                "file": "/nope/project/src/main.mw",
                "segments": "counter",
                "limit": 1,
            }),
            json!({
                "file": "/nope/project/src/main.mw",
                "segments": [],
                "limit": 1,
            }),
            json!({
                "file": "/nope/project/src/main.mw",
                "segments": [{ "kind": "root", "store_catalog_id": "cat_00000000000000000000000000000001" }],
                "limit": 0,
            }),
            json!({
                "file": "/nope/project/src/main.mw",
                "segments": [{ "kind": "root", "value": "counter" }],
                "limit": 1,
            }),
        ] {
            let error = call("mw_data_children", &arguments, &policy)
                .expect_err("mw_data_children must validate typed request arguments");
            assert!(
                error.contains("mw_data_children"),
                "invalid data children args should name the tool: {error}"
            );
        }
    }

    #[test]
    fn mw_data_read_rejects_invalid_typed_arguments() {
        let policy = Policy::new(true, false, mcp::DEFAULT_RUN_BUDGET);
        for arguments in [
            json!({
                "file": "/nope/project/src/main.mw",
                "segments": "counter",
            }),
            json!({
                "file": "/nope/project/src/main.mw",
                "segments": [],
            }),
            json!({
                "file": "/nope/project/src/main.mw",
                "segments": [{ "kind": "root", "store_catalog_id": "cat_00000000000000000000000000000001" }],
                "preview_limit": 0,
            }),
            json!({
                "file": "/nope/project/src/main.mw",
                "segments": [{ "kind": "root", "value": "counter" }],
            }),
        ] {
            let error = call("mw_data_read", &arguments, &policy)
                .expect_err("mw_data_read must validate typed request arguments");
            assert!(
                error.contains("mw_data_read"),
                "invalid data read args should name the tool: {error}"
            );
        }
    }

    #[test]
    fn tool_arguments_reject_non_array_run_args() {
        let policy = Policy::new(false, false, mcp::DEFAULT_RUN_BUDGET);
        let error = call(
            "mw_run",
            &json!({
                "file": "/nope/project/src/main.mw",
                "entry": "app::main",
                "args": 1,
            }),
            &policy,
        )
        .expect_err("non-array run args must be a protocol argument error");
        assert!(
            error.contains("args"),
            "mw_run should report the non-array args argument: {error}"
        );
    }

    #[test]
    fn optional_string_arguments_reject_concrete_non_strings() {
        let policy = Policy::new(false, false, mcp::DEFAULT_RUN_BUDGET);
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
            let error = call(tool, &arguments, &policy)
                .expect_err(&format!("{tool} should reject non-string `{key}`"));
            assert!(
                error.contains(key),
                "{tool} should report the malformed `{key}` argument: {error}"
            );
        }
    }

    #[test]
    fn optional_string_nulls_remain_absent() {
        let policy = Policy::new(false, false, mcp::DEFAULT_RUN_BUDGET);

        let result = call(
            "mw_run",
            &json!({
                "file": "/x.mw",
                "mode": null,
                "entry": null,
                "args": [{ "name": "n", "value": { "kind": "int", "value": "1" } }],
            }),
            &policy,
        )
        .unwrap();
        assert_ne!(result["diagnostics"][0]["code"], "mcp.run.args");
        assert!(
            result["diagnostics"][0]["message"]
                .as_str()
                .is_some_and(
                    |message| message.contains("project") || message.contains("marrow.json")
                ),
            "null optional strings should be absent and valid typed args should reach project loading: {result}"
        );

        let error = call("mw_check", &json!({ "source": null }), &policy)
            .expect_err("null source should be absent, not a source snippet");
        assert!(
            error.contains("file") && error.contains("source"),
            "null source should leave mw_check with neither input: {error}"
        );
    }

    #[test]
    fn mw_run_rejects_numeric_typed_int_args() {
        let policy = Policy::new(false, false, mcp::DEFAULT_RUN_BUDGET);
        let result = call(
            "mw_run",
            &json!({
                "file": "/nope/project/src/main.mw",
                "entry": "app::main",
                "args": [
                    { "name": "value", "value": { "kind": "int", "value": 1 } }
                ],
            }),
            &policy,
        )
        .unwrap();
        assert_eq!(
            result["diagnostics"][0]["code"], "mcp.run.args",
            "mw_run numeric JSON entry ints must fail before project loading: {result}"
        );
    }

    #[test]
    fn mw_run_rejects_malformed_typed_args() {
        let policy = Policy::new(false, false, mcp::DEFAULT_RUN_BUDGET);
        let result = call(
            "mw_run",
            &json!({
                "file": "/nope/project/src/main.mw",
                "entry": "app::main",
                "args": [1],
            }),
            &policy,
        )
        .unwrap();
        assert_eq!(
            result["diagnostics"][0]["code"], "mcp.run.args",
            "mw_run malformed typed args must fail before project loading: {result}"
        );
    }

    #[test]
    fn mw_run_rejects_test_mode_args() {
        let policy = Policy::new(false, false, mcp::DEFAULT_RUN_BUDGET);
        let result = call(
            "mw_run",
            &json!({
                "file": "/nope/project/src/main.mw",
                "mode": "test",
                "args": [
                    { "name": "value", "value": { "kind": "int", "value": "1" } }
                ],
            }),
            &policy,
        )
        .unwrap();
        assert_eq!(
            result["diagnostics"][0]["code"], "mcp.run.args",
            "mw_run test args must be rejected before project loading: {result}"
        );
    }

    #[test]
    fn initialize_result_advertises_tools_and_the_protocol_version() {
        let result = initialize_result(PROTOCOL_VERSION);
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "marrow-mcp");
    }

    #[test]
    fn protocol_version_negotiation_echoes_supported_and_falls_back_otherwise() {
        // A client that requests a version this server supports gets it echoed.
        assert_eq!(
            negotiate_protocol_version(Some(PROTOCOL_VERSION)),
            PROTOCOL_VERSION
        );
        assert_eq!(negotiate_protocol_version(Some("2025-03-26")), "2025-03-26");
        // A client that requests an unsupported version, or none, gets the latest.
        assert_eq!(
            negotiate_protocol_version(Some("2020-01-01")),
            PROTOCOL_VERSION
        );
        assert_eq!(negotiate_protocol_version(None), PROTOCOL_VERSION);
        // The echoed version is always one the server actually supports.
        assert!(SUPPORTED_PROTOCOL_VERSIONS.contains(&PROTOCOL_VERSION));
    }
}
