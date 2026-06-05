# LSP Fact Consumption Ledger

This ledger records what `marrow-lsp` may treat as semantic authority while
Marrow core is moving to v0.1. It is maintained through the product-surface
truth lane in
[`production-readiness-roadmap.md`](production-readiness-roadmap.md). The rule
is direct: if a fact belongs to Marrow and Marrow does not expose it, LSP, MCP,
DAP, and VS Code must delete the surface, block it, or render an explicitly
presentation-only view. They must not reconstruct language, catalog, runtime,
or storage meaning locally.

## Status Definitions

| Status | Meaning |
| --- | --- |
| `ready` | The surface consumes current public Marrow facts and does not decide language, catalog, storage, or runtime semantics locally. |
| `presentation-only` | The surface is UI over source text, diagnostics, transport output, packaging, or bounded debug/admin data. It is not a stable semantic contract. If users can invoke it, the contract must be explicit, bounded, tested, and impossible to confuse with production semantics. |
| `blocked-on-marrow` | The surface needs a Marrow-owned fact that is not exposed, and the executable feature path is absent or returns a stable unavailable result. |
| `deleted` | The prior implementation depended on removed or private Marrow details and has no active runtime/editor path. |

## Current Surface Matrix

| Surface | Status | Current authority | Missing Marrow facts / rule |
| --- | --- | --- | --- |
| LSP initialize/capability advertisement | `ready` | Static capability map in the LSP transport | Capabilities must not imply production semantic contracts beyond the rows below. |
| LSP text sync and diagnostics publish | `ready` | Full-document sync, Marrow diagnostics, spans, severity, and diagnostic codes | LSP renders Marrow diagnostics only. Logic and tests must not infer meaning from diagnostic prose. |
| Workspace analysis cache | `presentation-only` | `marrow_project`, `analyze_project`, `AnalysisSnapshot`, `CheckedProgram`, `build_binding_index` | Needs versioned analysis snapshots with source digest, catalog epoch, project config digest, and store/runtime generation before cached or live facts can be production-stable. |
| Diagnostics | `ready` | Marrow diagnostics, spans, severity, and diagnostic codes | LSP renders Marrow diagnostics only. Logic and tests must not infer meaning from diagnostic prose. |
| Formatting | `ready` | `marrow_syntax::format_source` and parse-error refusal | No local formatter semantics. |
| Hover: locals, params, functions, resources, enums, builtins, modules, roots | `presentation-only` | `type_at`, `BindingIndex`, `resolve`, checked functions/resources/enums, direct effect facts, standard-library descriptors | Needs query-native hover facts for declaration names, module paths, type annotations, builtins, saved roots, enum member paths, docs, checked saved places, transitive effects, and versioned live-data binding before it can be a production semantic contract. Local token walking is presentation only and must shrink as Marrow facts appear. |
| Go-to definition, references, prepare rename, rename edits | `presentation-only` | `BindingIndex`, `SymbolRef`, `SymbolKind`, `RenameSafety`, `resolve`, parsed source for outline only | Needs durable identity facts for resources, stores, fields, layers, indexes, enum members, aliases, module prefixes, saved roots, and type annotations before it can be a production semantic contract. Source spelling is not durable identity. Saved-data-backed edits refuse rather than guessing. |
| Document and workspace symbols | `presentation-only` | Parsed source plus checked program facts where available | Outline/search display current source shape only. They are not catalog authority. |
| Semantic tokens | `presentation-only` | Lexer/parser output, binding index where available, standard-library descriptors | Resolved role coloring needs canonical role facts for builtins, operators, modules, type annotations, saved roots, fields, layers, indexes, enum members, and default-library markers. |
| Completion | `presentation-only` | `scope_at`, `resolve`, checked schemas/functions, syntax recovery, standard-library descriptors | Needs checker-owned completion context facts for namespace, expected type, active path, aliases, and recovery mode before it can be a production semantic contract. Do not infer durable context from nearby tokens. |
| Signature help | `presentation-only` | Checked function/schema descriptors and source call shape | Needs checked active-call facts, argument position, suppression rules, builtin signatures, and resource constructor signatures before it can be a production semantic contract. |
| Custom LSP `marrow/savedRoots` for Data Roots | `presentation-only` | Root-only saved-data listing when `marrow.liveData` is enabled; no child/path traversal | Needs catalog-bound saved-place identity, typed child segments, paging/cursor facts, store generation, and stable data DTOs. TypeScript must remain a view layer and must not derive child paths. |
| Custom LSP `marrow/dataIntegrity` | `presentation-only` | Advisory debug/admin command output only where backed by current public tooling | Needs catalog/store identity, generation, drift witnesses, and typed repair facts before it can be production validation. |
| Live saved-data hover and live record counts | `deleted` | None | Removed until Marrow exposes checked saved-place queries, typed store summaries, and source/store generation boundaries. |
| LSP stdio transport | `ready` | Transport over `marrow-lsp-core` | Inherits the core feature statuses above. |
| MCP initialize/tools transport | `ready` | Thin stdio adapter over `marrow-lsp-core` | Individual tools carry their own status. |
| MCP `mw_check` | `ready` | Marrow project/snippet diagnostics and diagnostic codes | No local checker semantics. |
| MCP `mw_type_at` | `ready` | `marrow_check::type_at` | No local type inference. |
| MCP `mw_complete` | `presentation-only` | Bounded transport envelope over the current editor completion helper | Needs typed Marrow completion DTOs and query-native context facts before it can be a production completion API. |
| MCP `mw_resource_schema` | `presentation-only` | Unambiguous named resource projection from current checked schema facts | `name` is required; all-resource listing is unavailable until Marrow exposes paged catalog/schema DTOs. Duplicate same-name resources return `mcp.resourceSchema.identity` instead of inventing identity from source spelling. Needs catalog-bound resource/store/member identity and typed protocol DTOs. |
| MCP `mw_saved_roots` | `presentation-only` | Root listing only when data access is enabled | Needs typed Marrow data DTOs, child identity, cursor/page facts, and generation facts before expansion or value reads can be exposed. |
| MCP `mw_data_integrity` | `presentation-only` | Capped current-schema advisory over the real store when data access is enabled | Needs catalog/store identity, generation, drift witnesses, and typed repair facts before it can be production validation. |
| MCP saved get/children tools | `deleted` | None | Removed until Marrow exposes typed child identity, cursor/page facts, and generation facts. |
| MCP `mw_run` pure execution/test helper | `presentation-only` | Checked zero-argument entry-call API over a fresh in-memory `TreeStore` and locked host | Runs only after normal check succeeds and no accepted-catalog baseline is pending. Explicit `entry` strings are accepted only as a presentation helper and are not stable production entry identity. Missing entry and non-empty args return stable blockers. Production run/debug needs canonical entry facts, transitive effects, durable scope, transaction facts, runtime generation, and typed DTOs. |
| MCP `mw_run` durable catalog admission | `blocked-on-marrow` | Stable diagnostic envelope; no catalog or store write | `mw_run` does not call Marrow's production catalog writer. Read-only run admission over pending catalog identity needs Marrow facts. |
| DAP initialize and shutdown transport | `ready` | DAP protocol handling and static capability response | Advertised capabilities must stay limited to implemented rows. Hover evaluate is not advertised. |
| DAP launch with configured `defaultEntry` | `presentation-only` | Project analysis plus Marrow checked entry-call runtime over an in-memory store | Launch blocks projects with pending accepted catalog identity. Production debug launch needs canonical entry selection, durable scope, transaction, store/runtime generation, and debug admission facts. |
| DAP explicit `entry` launch argument | `blocked-on-marrow` | Stable failed launch response | VS Code does not expose this property. It remains unavailable until Marrow exposes canonical function-entry facts. |
| DAP non-empty launch `args` | `blocked-on-marrow` | Stable failed launch response | Needs typed launch argument decoding facts. VS Code schema caps args at zero. |
| DAP `setBreakpoints` line arming | `blocked-on-marrow` | Advisory unverified breakpoint response; no line is armed | Needs canonical source mapping and stop-point facts. |
| DAP stop-on-entry and stepping | `presentation-only` | Statement spans from the checked runtime hook | Needs canonical debugger source/stop facts before production breakpoint/source semantics. |
| DAP threads, stack trace, scopes, locals, expandable locals | `presentation-only` | Single-thread debug adapter state and public runtime values where available | Runtime value expansion facts and stable source mapping are still missing; values carry blocked contract metadata rather than production type claims. |
| DAP durable variables/watch/evaluate/hover evaluate | `blocked-on-marrow` | Stable failed responses; no durable path parser or expression evaluator | Needs durable watch/path facts, expression/evaluate facts, typed value expansion, and source/store generation facts. |
| DAP disconnect/terminate/output | `ready` | Adapter lifecycle and captured runtime output | No language semantics. |
| VS Code activation and binary launcher | `presentation-only` | TypeScript starts bundled or configured binaries | Development path resolution is editor-launch plumbing only, not language authority. |
| VS Code TextMate grammar and language configuration | `presentation-only` | Lexical grammar and editor defaults | Grammar is a visual layer only; semantic meaning remains in Marrow/LSP. |
| VS Code Marrow Data Roots view and refresh command | `presentation-only` | Root-only custom LSP request | View lists roots only, requires `marrow.liveData`, and exposes no child paths or stored values. |
| VS Code data integrity command | `presentation-only` | Advisory custom LSP request | Debug/admin advisory only, not production validation or repair. |
| VS Code debug contribution and F5 snippets | `presentation-only` | Default-entry launch wiring, zero args, stop-on-entry/stepping/advisory breakpoint UI | No explicit `entry` property is exposed. Non-empty args and line breakpoint arming remain blocked. |
| VS Code packaging/bundling | `ready` | Package scripts and bundled binaries | Package gates and audit state must be reported in the package lane. |

## Blocked Marrow Facts

| Blocker | Required fact/API | Surfaces blocked |
| --- | --- | --- |
| Versioned snapshots | Source digest, config digest, catalog epoch, store/runtime generation | Workspace, live data, Data Roots, MCP data tools |
| Catalog-backed identities | Stable typed IDs for resources, stores, fields, layers, indexes, enum members, aliases, and generated identity constructors | Hover, navigation, rename, semantic tokens, completion, data tools, DAP |
| Split resource/store facts | Separate facts for resource schema, store binding, root, key shape, and store-backed roots | Data Roots, saved-root hover, completion, navigation |
| Query-native source facts | Checked hover/navigation/completion/signature facts for declarations, module paths, type annotations, docs, builtins, operators, enum paths, and saved roots | Source tooling |
| Checked saved places and effects | Lowered saved-place identity, write plans, durable reads/writes, typed runtime values, transitive effects, and durable scope | Data tools, DAP, run/debug UI |
| Debugger facts | Stop points, entry selection, typed launch args, expression evaluation, value expansion, durable watch targets, and typed path segments | DAP |
| Store admin facts | Typed child identity, cursor/page identity, index and scan facts, generation, drift witnesses, and repair facts | Data Roots, MCP data tools, data integrity |
| Typed protocol DTOs | Catalog-epoch-bound DTOs for resources, stores, paths, values, diagnostics, and admin views | MCP, DAP, VS Code data views |

## Removed Or Blocked Surfaces

The following prior surfaces have no production authority and must not reappear
without a new Marrow fact and a review-gated lane:

- Local durable-data classifiers and private store-key readers.
- Editor or transport options that opt into raw durable-data inspection.
- Scalar launch/run argument decoders outside Marrow.
- Live saved-data hover and live record-count hints.
- TypeScript child-path derivation for Data Roots.
- Local expression evaluation and durable watch parsing in DAP.
- VS Code launch snippets or configuration properties for explicit DAP `entry`
  strings.
- DAP line-breakpoint arming from source lines.
- MCP all-resource schema listing without a named resource or paged Marrow DTO.
- Source-spelling identities for saved roots, resource members, indexes, enum
  members, aliases, or physical store keys.

## Handoff Rule

When a lane discovers a missing fact, it records the exact Marrow fact needed and
returns an unavailable contract or deletes the surface. It does not add local
classifiers, compatibility switches, untyped path protocols, message parsing, or
alternate semantic models in `marrow-lsp`.
