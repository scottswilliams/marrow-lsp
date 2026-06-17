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

## Current Marrow API Rebase Notes

Several old blockers predate current Marrow v0.1 APIs. These APIs do not make
the current LSP, MCP, DAP, or VS Code surfaces `ready` by themselves; they mark
the next work as LSP adoption unless a lane proves a narrower upstream gap:

- Runtime/session adoption candidates:
  `marrow_run::ProjectSession`, `ProjectOpen`, `ProjectMode`,
  `SessionEntry`, `CheckedEntryCall`, `run_entry`, `run_entry_with_debugger`,
  and read-only expression evaluation.
- Checked-program adoption candidates:
  source digests, evolution digests, read-only context digests,
  entry footprints, entry cost shapes, entry store-open modes, runtime
  programs, and checked read-only expressions.
- Data tooling adoption candidates:
  `resolve_data_query`, `resolve_source_text_data_query`, `read_data_query`,
  `render_data_query_value`, `data_children`, `walk_data`,
  `data_roots_in_store`, `count_data_records`, and integrity sampling or
  visiting APIs.
- Store/admin adoption candidates:
  `CommitMetadata`, store UID, catalog snapshot digest, read-only snapshots,
  and catalog snapshot reads.

Rows below that mention these APIs are current implementation limits, not
proof that Marrow lacks the fact.

## LSP Adoption Candidates

These surfaces are safe in their current narrowed form, but must no longer be
treated as upstream API requests until an LSP lane proves a more specific gap.

| Surface | Current safety contract | Marrow API candidates | Next LSP lane |
| --- | --- | --- | --- |
| DAP configured `defaultEntry` session plumbing | Configured default-entry launch remains a narrowed debug helper | `ProjectSession`, `SessionEntry`, checked runtime program facts | Explore session-backed launch for the configured-entry path only. Explicit entry strings and non-empty protocol args remain blocked until Marrow exposes stable canonical DTOs. |
| Data Roots children and values | Root-only listing behind `marrow.liveData` | `resolve_data_query`, `data_children`, `read_data_query`, `walk_data`, value rendering APIs | Add a shared core data-query layer with bounded paging and typed errors. |
| MCP data children and reads | Deleted old tools; no active untyped path protocol | `resolve_data_query`, `read_data_query`, `data_children`, `walk_data`, value rendering APIs | Reintroduce only through the shared core data-query layer. |
| Data integrity samples | Advisory capped scan only | integrity sampling and visiting APIs, store metadata APIs | Carry generation metadata and keep repair or validation claims blocked until Marrow exposes repair facts. |

## Current Surface Matrix

| Surface | Status | Current authority | Missing Marrow facts / rule |
| --- | --- | --- | --- |
| LSP initialize/capability advertisement | `ready` | Static capability map in the LSP transport | Capabilities must not imply production semantic contracts beyond the rows below. |
| LSP text sync and diagnostics publish | `ready` | Full-document sync, Marrow diagnostics, spans, severity, and diagnostic codes | LSP renders Marrow diagnostics only. Logic and tests must not infer meaning from diagnostic prose. |
| Workspace analysis cache | `presentation-only` | `marrow_project`, `analyze_project`, `AnalysisSnapshot`, `AnalysisIdentity`, `CheckedProgram`, `build_binding_index`, checked source digests | Needs LSP adoption of config, catalog, store, and runtime generation boundaries before cached or live facts can be production-stable. |
| Diagnostics | `ready` | Marrow diagnostics, spans, severity, and diagnostic codes | LSP renders Marrow diagnostics only. Logic and tests must not infer meaning from diagnostic prose. |
| Formatting | `ready` | `marrow_syntax::format_source` and parse-error refusal | No local formatter semantics. |
| Hover: locals, params, functions, resources, enums, builtins, modules, roots | `presentation-only` | `type_at`, `BindingIndex`, `resolve`, checked functions/resources/enums, direct effect facts, standard-library descriptors | Needs query-native hover facts for declaration names, module paths, type annotations, builtins, saved roots, enum member paths, docs, checked saved places, transitive effects, and versioned live-data binding before it can be a production semantic contract. Local token walking is presentation only and must shrink as Marrow facts appear. |
| Go-to definition, references, prepare rename, rename edits | `presentation-only` | `BindingIndex`, `SymbolRef`, `SymbolKind`, `RenameSafety`, `resolve`, parsed source for outline only | Needs durable identity facts for resources, stores, fields, layers, indexes, enum members, aliases, module prefixes, saved roots, and type annotations before it can be a production semantic contract. Source spelling is not durable identity. Saved-data-backed edits refuse rather than guessing. |
| Document and workspace symbols | `presentation-only` | Parsed source plus checked program facts where available | Outline/search display current source shape only. They are not catalog authority. |
| Semantic tokens | `presentation-only` | Lexer/parser output, binding index where available, standard-library descriptors | Resolved role coloring needs canonical role facts for builtins, operators, modules, type annotations, saved roots, fields, layers, indexes, enum members, and default-library markers. |
| Completion | `presentation-only` | `scope_at`, `resolve`, checked schemas/functions, syntax recovery, standard-library descriptors | Needs checker-owned completion context facts for namespace, expected type, active path, aliases, and recovery mode before it can be a production semantic contract. Do not infer durable context from nearby tokens. |
| Signature help | `presentation-only` | Checked function/schema descriptors and source call shape | Needs checked active-call facts, argument position, suppression rules, builtin signatures, and resource constructor signatures before it can be a production semantic contract. |
| Custom LSP `marrow/savedRoots` for Data Roots | `presentation-only` | Root-only saved-data listing when `marrow.liveData` is enabled; no child/path traversal | Marrow now exposes data query, child, walk, render, count, and cursor APIs. The current LSP still consumes only roots. Expansion is LSP adoption work and must keep TypeScript as a view layer. |
| Custom LSP `marrow/dataIntegrity` | `presentation-only` | Advisory debug/admin command output only where backed by current public tooling | Marrow exposes integrity sampling and visiting APIs. Production validation still needs catalog/store generation boundaries, drift witnesses, and typed repair facts. |
| Live saved-data hover and live record counts | `deleted` | None | Removed until LSP adopts Marrow data query/count APIs and carries source/store generation boundaries. |
| LSP stdio transport | `ready` | Transport over `marrow-lsp-core` | Inherits the core feature statuses above. |
| MCP initialize/tools transport | `ready` | Thin stdio adapter over `marrow-lsp-core` | Individual tools carry their own status. |
| MCP `mw_check` | `ready` | Marrow project/snippet diagnostics and diagnostic codes | No local checker semantics. |
| MCP `mw_type_at` | `ready` | `marrow_check::type_at` | No local type inference. |
| MCP `mw_complete` | `presentation-only` | Bounded transport envelope over the current editor completion helper | Needs typed Marrow completion DTOs and query-native context facts before it can be a production completion API. |
| MCP `mw_resource_schema` | `presentation-only` | Unambiguous named resource projection from current checked schema facts | `name` is required; all-resource listing is unavailable until Marrow exposes paged catalog/schema DTOs. Duplicate same-name resources return `mcp.resourceSchema.identity` instead of inventing identity from source spelling. Needs catalog-bound resource/store/member identity and typed protocol DTOs. |
| MCP `mw_saved_roots` | `presentation-only` | Root listing only when data access is enabled | Marrow now exposes data query, child, walk, render, count, and cursor APIs. Expansion and value reads are LSP adoption work through shared typed DTOs, not TypeScript or MCP path inference. |
| MCP `mw_data_integrity` | `presentation-only` | Capped current-schema advisory over the real store when data access is enabled | Marrow exposes integrity sampling and visiting APIs. Production validation still needs catalog/store generation boundaries, drift witnesses, and typed repair facts. |
| MCP saved get/children tools | `deleted` | None | The old path was removed. Reintroduce only through Marrow's data query, child, walk, render, cursor, and generation APIs with bounded paging. |
| MCP `mw_run` pure execution/test helper | `presentation-only` | Checked zero-argument entry-call API over a fresh in-memory `TreeStore` and locked host | Runs only after normal check succeeds and no accepted-catalog baseline is pending. Current code blocks non-empty args and treats explicit `entry` as presentation. Current Marrow session APIs do not satisfy this helper's forced fresh-memory/no-real-store-open contract. |
| MCP `mw_run` durable catalog admission | `blocked-on-marrow` | Stable unavailable envelope; no catalog or store write from the current MCP helper | Needs a Marrow run/session API that can force checked zero-argument execution against a fresh in-memory store without opening, reading, copying, or writing the project store, plus canonical entry identity and typed run-argument DTOs. |
| DAP initialize and shutdown transport | `ready` | DAP protocol handling and static capability response | Advertised capabilities must stay limited to implemented rows. Hover evaluate is not advertised. |
| DAP launch with configured `defaultEntry` | `presentation-only` | Project analysis plus Marrow checked entry-call runtime over an in-memory store | Launch blocks projects with pending accepted catalog identity. Production debug launch must evaluate Marrow session APIs for configured entry, admission, store generation, and debug hooks without graduating explicit entry or protocol args. |
| DAP explicit `entry` launch argument | `blocked-on-marrow` | Stable failed launch response; VS Code does not expose this property | Current Marrow session helpers are string-based and do not provide a stable canonical entry DTO for DAP. Keep blocked until Marrow exposes canonical entry identity for protocol launch. |
| DAP non-empty launch `args` | `blocked-on-marrow` | Stable failed launch response; VS Code schema caps args at zero | Current Marrow text-argument helpers are not a stable typed DAP launch-argument DTO. Keep blocked until Marrow exposes canonical typed launch argument facts. |
| DAP `setBreakpoints` line arming | `blocked-on-marrow` | Advisory unverified breakpoint response; no line is armed | Needs canonical source mapping and stop-point facts. |
| DAP stop-on-entry and stepping | `presentation-only` | Statement spans from the checked runtime hook | Needs canonical debugger source/stop facts before production breakpoint/source semantics. |
| DAP threads, stack trace, scopes, locals, expandable locals | `presentation-only` | Single-thread debug adapter state and public runtime values where available | Runtime value expansion facts and stable source mapping are still missing; values carry blocked contract metadata rather than production type claims. |
| DAP durable variables/watch/evaluate/hover evaluate | `blocked-on-marrow` | Stable failed responses; no durable path parser, frame-scope evaluator, or production value explorer | Marrow exposes read-only expression evaluation and data query candidates, but DAP still needs canonical frame-scope, durable watch/path, typed value expansion, stop source, and source/store generation facts before production debugger evaluation. |
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
| Versioned snapshots | Config digest plus catalog, store, and runtime generation carried through LSP snapshots | Workspace, live data, Data Roots, MCP data tools |
| Catalog-backed identities | Stable typed IDs for resources, stores, fields, layers, indexes, enum members, aliases, and generated identity constructors | Hover, navigation, rename, semantic tokens, completion, data tools, DAP |
| Split resource/store facts | Separate facts for resource schema, store binding, root, key shape, and store-backed roots | Data Roots, saved-root hover, completion, navigation |
| Query-native source facts | Checked hover/navigation/completion/signature facts for declarations, module paths, type annotations, docs, builtins, operators, enum paths, and saved roots | Source tooling |
| Checked saved places and effects | Lowered write plans, durable writes, typed runtime values, transitive effects, and durable scope not covered by current session/data APIs | Data tools, DAP, run/debug UI |
| Forced-memory run session | Stable run/session mode for checked zero-argument execution over a fresh in-memory store without opening, reading, copying, or writing the configured project store | MCP `mw_run` production graduation |
| Debugger facts | Stop points, stable protocol entry identity, typed launch args, frame scopes, production expression evaluation, value expansion, durable watch targets, and typed path segments | DAP |
| Store admin facts | Generation boundaries, drift witnesses, and repair facts beyond current query, walk, count, and integrity sampling APIs | Data Roots, MCP data tools, data integrity |
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
