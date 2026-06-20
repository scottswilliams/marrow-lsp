# LSP Fact Consumption Ledger

This ledger records what `marrow-lsp` may treat as semantic authority while
Marrow core is moving to v0.1. It is the surface-status authority for the
release contract in
[`production-readiness-roadmap.md`](production-readiness-roadmap.md). The rule is
direct: if a fact belongs to Marrow and Marrow does not expose it, LSP, MCP, DAP,
and VS Code must delete the surface, block it, or render an explicitly
presentation-only view. They must not reconstruct language, catalog, runtime, or
storage meaning locally.

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
  saved-data path resolution, source-text path resolution, read, render,
  child-page, walk, root-listing, count, and integrity sampling or visiting
  APIs.
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
| DAP launch entry invocation | Configured and explicit-entry launch uses isolated Marrow project sessions and remains a narrowed debug helper | `SessionEntry`, canonical entry DTOs, typed launch-argument DTOs, debugger facts | Entry selectors, typed args, plain-line stop points, and local runtime value expansion are admitted through Marrow facts. Production graduation still needs virtual-source, store-generation, frame-scope evaluation, and serve/attach facts. |
| Saved Resource Inspector children and values | Stable Marrow saved-data DTOs, typed child expansion, bounded child pages, and bounded value previews behind `marrow.liveData` | Shared saved-data path resolution, child, read, walk, render, cursor, and preview APIs | Root listing, child pages, and value reads carry Marrow `store_snapshot` metadata. Production live-data use still needs versioned source/store generation, watch/refresh, integrity, repair, serve, and attach contracts. |
| MCP data roots and children | Stable Marrow saved-data DTOs and typed saved-data path protocol through the shared core | Shared saved-data path resolution, root-listing, child, walk, render, and cursor APIs | Root listing and child pages carry generation metadata. Production serving still needs versioned source/store generation, integrity, repair, watch, serve, and attach contracts. |
| Data integrity samples | Advisory capped scan with Marrow `store_snapshot` metadata | integrity sampling and visiting APIs, store metadata APIs | Keep compatibility, repair, and validation claims blocked until Marrow exposes source/store compatibility verdicts, drift witnesses, typed repair facts, and stable production integrity DTOs. |

## Current Surface Matrix

| Surface | Status | Current authority | Missing Marrow facts / rule |
| --- | --- | --- | --- |
| LSP initialize/capability advertisement | `ready` | Static capability map in the LSP transport | Capabilities must not imply production semantic contracts beyond the rows below. |
| LSP text sync and diagnostics publish | `ready` | Full-document sync, Marrow diagnostics, spans, severity, and diagnostic codes | LSP renders Marrow diagnostics only. Logic and tests must not infer meaning from diagnostic prose. |
| Workspace analysis cache | `presentation-only` | `marrow_project`, `analyze_project`, `AnalysisSnapshot`, `AnalysisIdentity`, `CheckedProgram`, `build_binding_index`, checked source digests | Needs LSP adoption of config, catalog, store, and runtime generation boundaries before cached or live facts can be production-stable. |
| Diagnostics | `ready` | Marrow diagnostics, spans, severity, and diagnostic codes | LSP renders Marrow diagnostics only. Logic and tests must not infer meaning from diagnostic prose. |
| Formatting | `ready` | `marrow_syntax::format_source` and parse-error refusal | No local formatter semantics. |
| Hover: locals, params, functions, resources, enums, builtins, modules, roots | `presentation-only` | `type_at`, `BindingIndex`, `resolve`, `AnalysisSnapshot` catalog use-sites/declarations for enum type annotations, checked functions/resources/enums, direct effect facts, standard-library descriptors | Enum type-annotation leaf hovers consume canonical catalog facts. The broader surface still needs checker-owned typed hover facts for declaration names, module paths, other type annotations, builtins, saved roots, enum member paths, docs, checked saved places, transitive effects, and versioned live-data binding before it can be a production semantic contract. Local token walking is presentation only and must shrink as Marrow facts appear. |
| Go-to definition, references, prepare rename, rename edits | `presentation-only` | `BindingIndex`, `SymbolRef`, `SymbolKind`, `RenameSafety`, `resolve`, `AnalysisSnapshot` catalog use-sites/declarations for saved roots, resource members, store indexes, and enum type annotations; parsed source for outline only | Saved roots, resource members, store indexes, and enum type-annotation leaves use canonical catalog spans. The broader surface still needs durable identity facts for complete resource/store identity, enum members, aliases, module prefixes, rename edits, and non-enum type annotations before it can be a production semantic contract. Source spelling is not durable identity. Saved-data-backed edits refuse rather than guessing. |
| Document and workspace symbols | `presentation-only` | Parsed source, checked functions/constants, and `AnalysisSnapshot` catalog declarations for catalog-backed symbols | Outline/search display current source shape only. Catalog-backed workspace symbols consume canonical declaration spans, but the broader surface still needs checker-owned symbol DTOs for detail, hierarchy, filtering, and production-stable symbol search. |
| Semantic tokens | `presentation-only` | Lexer/parser output, binding index where available, standard-library descriptors | Resolved role coloring needs canonical role facts for builtins, operators, modules, type annotations, saved roots, fields, layers, indexes, enum members, and default-library markers. |
| Completion | `presentation-only` | `scope_at`, `resolve`, checked schemas/functions, syntax recovery, standard-library descriptors | Needs checker-owned completion context facts for namespace, expected type, active path, aliases, and recovery mode before it can be a production semantic contract. Do not infer durable context from nearby tokens. |
| Signature help | `presentation-only` | Checked function/schema descriptors and source call shape | Needs checked active-call facts, argument position, suppression rules, builtin signatures, and resource constructor signatures before it can be a production semantic contract. |
| Custom LSP `marrow/savedRoots` | `presentation-only` | Saved-root listing with Marrow `store_snapshot` metadata when `marrow.liveData` is enabled | Kept as the root listing entry point for the Saved Resource Inspector. TypeScript must not derive child paths from source text or render key labels itself. Production use still needs versioned source/store generation, watch/refresh, serve, and attach contracts. |
| Custom LSP `marrow/dataChildren` and `marrow/dataRead` | `presentation-only` | Stable Marrow saved-data DTOs, typed path segments, bounded child pages, Marrow-rendered labels, and Marrow bounded value previews over the committed native dev store; child pages and value reads carry Marrow `store_snapshot` metadata; invalid empty paths and zero limits fail before project or store reads | Production live saved-data inspection still needs versioned source/store generation, watch/refresh, integrity, repair, serve, and attach contracts. |
| Custom LSP `marrow/dataIntegrity` | `presentation-only` | Advisory debug/admin command output with Marrow `store_snapshot` metadata only where backed by current public tooling | Production validation still needs source/store compatibility verdicts, drift witnesses, typed repair facts, and stable production integrity DTOs. |
| Live saved-data hover and live record counts | `deleted` | None | Removed until LSP carries source/store generation boundaries for saved-data reads and counts. |
| LSP stdio transport | `ready` | Transport over `marrow-lsp-core` | Inherits the core feature statuses above. |
| MCP initialize/tools transport | `ready` | Thin stdio adapter over `marrow-lsp-core` | Individual tools carry their own status. |
| MCP request `params` envelope | `ready` | MCP stdio transport validation of supported request `params` containers | Supported methods accept absent, `null`, and object params, and reject concrete non-object params with `INVALID_PARAMS`. Unknown methods stay `METHOD_NOT_FOUND`; notifications without `id` draw no reply. No language semantics. |
| MCP `mw_check` | `ready` | Marrow project/snippet diagnostics and diagnostic codes | No local checker semantics. |
| MCP `mw_type_at` | `ready` | `marrow_check::type_at` | No local type inference. |
| MCP `mw_complete` | `presentation-only` | Bounded transport envelope over the current editor completion helper | Needs typed Marrow completion DTOs and checker-owned context facts before it can be a production completion API. |
| MCP `mw_resource_schema` | `presentation-only` | Unambiguous named resource projection from current checked schema facts | `name` is required; all-resource listing is unavailable until Marrow exposes paged catalog/schema DTOs. Duplicate same-name resources return `mcp.resourceSchema.identity` instead of inventing identity from source spelling. Needs catalog-bound resource/store/member identity and typed protocol DTOs. |
| MCP `mw_saved_roots` | `presentation-only` | Root listing with Marrow `store_snapshot` metadata when data access is enabled | Kept as a root listing helper. Production data serving still needs versioned source/store generation, integrity, repair, watch, serve, and attach contracts. |
| MCP `mw_data_children` and `mw_data_read` | `presentation-only` | Stable Marrow saved-data DTOs, typed path segments, bounded child pages, Marrow-rendered value previews with bounded rendering budgets, and Marrow `store_snapshot` metadata over the real store when data access is enabled; invalid empty paths and zero limits fail before project or store reads | No local saved-data semantics remain for child expansion or value preview rendering. `mw_data_read` does not claim a bounded production read contract because Marrow presence detection for non-leaf paths can still inspect siblings. Production data serving still needs versioned source/store generation, bounded presence/read facts, integrity, repair, watch, serve, and attach contracts. |
| MCP `mw_data_integrity` | `presentation-only` | Capped current-schema advisory plus Marrow `store_snapshot` metadata over the real store when data access is enabled | Production validation still needs source/store compatibility verdicts, drift witnesses, typed repair facts, and stable production integrity DTOs. |
| MCP saved get/children tools | `deleted` | None | The old untyped path was removed. New saved-data inspection must stay on shared typed DTOs with bounded paging. |
| MCP `mw_run` pure execution/test helper | `presentation-only` | Marrow-owned fresh-memory project session, canonical entry descriptor invocation, typed run arguments, direct fresh-store test helper, and locked host | Run mode checks through the normal project path, then uses `ProjectOpen::with_fresh_memory_store()` so the configured project store is not opened and durable catalog state is not established. Production graduation still needs durable-scope/effect facts, transaction facts, runtime generation facts, and serve/attach execution boundaries. No durable-data execution claim. |
| DAP initialize and shutdown transport | `ready` | DAP protocol handling and static capability response | Advertised capabilities must stay limited to implemented rows. Conditional breakpoints are advertised; hover evaluate, hit conditions, and logpoints are not advertised. The DAP `initialized` event is emitted after launch preparation, when Marrow stop-point and debug-expression facts are available for configuration requests. |
| DAP request `arguments` envelope | `ready` | Supported requests validate the top-level `arguments` container before command handlers run: absent or `null` arguments are empty, object arguments are accepted, and concrete non-object arguments fail with `invalid-params`. Unsupported commands stay `unsupported-request`. | No language semantics. |
| DAP launch with configured `defaultEntry` | `presentation-only` | `marrow_run::ProjectSession` opened with isolated writes, canonical entry descriptor invocation, typed argument DTOs, runtime stop-point facts, launch-time `AnalysisSnapshot` identity-bound to the runtime session, runtime debug value facts, and the Marrow debugger hook | Configured-entry launch does not create accepted catalog state or native stores for absent data dirs, and writes stay isolated. Malformed launch envelope fields fail with `invalid-params`; optional `entry` and `stopOnEntry` `null` values remain absent/default. Existing native stores that would require catalog artifact repair are blocked until Marrow exposes read-only debug catalog admission facts. Local values and local debug expressions are Marrow-owned debug facts, but production debug launch still needs virtual-source, source/store-generation, and serve/attach facts before graduating beyond a narrowed debug helper. |
| DAP explicit `entry` launch argument | `presentation-only` | Explicit string selectors are handed to `ProjectOpen::with_entry_override`, then admitted by Marrow's current canonical entry descriptor before launch succeeds; concrete non-string values fail with `invalid-params` | The selector is a transport convenience, not durable identity. Production debug launch still inherits the DAP virtual-source, source/store-generation, and serve/attach blockers. |
| DAP non-empty launch `args` | `presentation-only` | Launch args decode to Marrow's typed entry DTOs and are admitted by `CheckedEntryCall::from_protocol_invocation`; non-array values, including `null`, fail with `invalid-params` | The adapter owns only JSON envelope validation. Marrow owns parameter names, catalog IDs, typed values, and admission against the checked program. Production debug launch still inherits the DAP virtual-source, source/store-generation, and serve/attach blockers. |
| DAP `setBreakpoints` plain line arming | `presentation-only` | Plain line breakpoints are checked against `CheckedRuntimeProgram::stop_points()` plus runtime file ids/paths. Verified stop-point lines are armed before the run thread starts and can be updated while the run is parked; lines outside Marrow stop points return an `invalid-params` breakpoint contract. Out-of-sequence pre-launch requests remain unverified and unarmed. Malformed source envelopes and non-array breakpoint lists fail with `invalid-params`; positive `sourceReference` sources fail with `unsupported-request` because the adapter exposes no virtual-source surface. | Virtual sources, serve/attach boundaries, and source/store generation are still outside this narrowed debug helper. |
| DAP conditional breakpoints | `presentation-only` | Non-empty `condition` expressions on verified Marrow stop-point lines are checked by `AnalysisSnapshot::checked_debug_expression` at that stop and must type as `bool`; armed conditions are evaluated on the parked run thread with `Frame::evaluate_debug_expression`, and the stop is taken only for `true`. Invalid or non-bool conditions return an `invalid-params` breakpoint contract and are not armed. | Hit conditions, logpoints, virtual sources, serve/attach boundaries, and source/store generation remain blocked. |
| DAP hit-condition and logpoint breakpoints | `blocked-on-marrow` | Inputs receive a distinct blocked contract and are not evaluated, echoed, or armed. | Needs canonical hit-count/logpoint semantics backed by Marrow facts. |
| DAP stop-on-entry and stepping | `presentation-only` | Statement spans and source file paths from the checked runtime hook. Malformed resume `threadId` envelopes fail with `invalid-params`. | Plain line breakpoints now consume Marrow stop-point facts; production debugger semantics still need virtual-source, store-generation, frame-scope evaluation, and serve/attach facts. |
| DAP threads, stack trace, scopes, locals, expandable locals | `presentation-only` | Single-thread debug adapter state, checked runtime source paths, and `Frame::debug_snapshot`/`DebugValue` for bounded local values and expandable local children. Malformed stack `threadId`, scope `frameId`, `variablesReference`, variable paging, and variable filter envelopes fail with `invalid-params`; scopes require a current stopped frame; unknown positive variable references remain empty for stale-reference tolerance. | Local variables no longer carry blocked runtime-value metadata, but threads, stack trace, scopes, virtual sources, and source/store generation remain outside a production debugger contract. |
| DAP watch/REPL local evaluate | `presentation-only` | While stopped, `watch`, `repl`, and absent-context expressions are checked against the stopped frame file/span by `AnalysisSnapshot::checked_debug_expression`, evaluated on the parked run thread with `Frame::evaluate_debug_expression`, and returned with the same `DebugValue` presentation and expandable-child contract as locals. Invalid expressions fail with `invalid-params`; runtime rejections fail with `runtime-error`; not-stopped requests fail with `invalid-state`. | Durable watch/path expressions, hover exact-frame semantics, virtual sources, serve/attach boundaries, and source/store generation remain blocked. |
| DAP durable variables/watch/path and hover evaluate | `blocked-on-marrow` | Durable variable references and `^` watch/path expressions keep the stable durable-data blocked contract; hover evaluate keeps the stable hover blocked contract and is not advertised. Malformed evaluate envelopes fail with `invalid-params`; no durable path parser or production value explorer is present. | DAP still needs canonical durable watch/path facts, hover exact-frame semantics, virtual sources, and source/store generation facts before those surfaces can graduate. |
| DAP disconnect/terminate/output | `ready` | Adapter lifecycle and captured runtime output | No language semantics. |
| VS Code activation and binary launcher | `presentation-only` | TypeScript starts bundled or configured binaries | Development path resolution is editor-launch plumbing only, not language authority. |
| VS Code TextMate grammar and language configuration | `presentation-only` | Lexical grammar and editor defaults | Grammar is a visual layer only; semantic meaning remains in Marrow/LSP. |
| VS Code Saved Resource Inspector and refresh command | `presentation-only` | Custom LSP saved-root, child-page, and bounded value-read requests using stable Marrow saved-data DTOs | Manual-refresh view expands committed native-store resources when `marrow.liveData` is enabled. Root, child-page, and value replies carry Marrow `store_snapshot` metadata; the view refuses to mix child pages or value reads from different snapshots and does not parse saved paths from source text, truncate values locally, watch uncommitted writes, watch debuggee state, or attach to served programs. |
| VS Code data integrity command | `presentation-only` | Advisory custom LSP request | Accepts but does not render or interpret Marrow `store_snapshot` metadata. Debug/admin advisory only, not production validation or repair. |
| VS Code debug contribution and F5 snippets | `presentation-only` | Launch schema for project, explicit entry selectors, typed args, exact string forms for int/date/bytes, stop-on-entry/stepping/plain line breakpoint UI, and DAP-advertised conditional breakpoint support | TypeScript only contributes schema/snippets and launches `marrow-dap`; it does not parse or validate Marrow entry values or breakpoint expressions. Hit-condition and logpoint breakpoints remain blocked. |
| VS Code packaging/bundling | `ready` | Package scripts and bundled binaries | Package gates and audit state must be reported in the package lane. |

## Blocked Marrow Facts

| Blocker | Required fact/API | Surfaces blocked |
| --- | --- | --- |
| Versioned snapshots | Config digest plus catalog, store, and runtime generation carried through LSP snapshots | Workspace, live data, Saved Resource Inspector, MCP data tools |
| Catalog-backed identities | Stable typed IDs for resources, stores, fields, layers, indexes, enum members, aliases, and generated identity constructors | Hover, navigation, rename, semantic tokens, completion, data tools, DAP |
| Split resource/store facts | Separate facts for resource schema, store binding, root, key shape, and store-backed roots | Saved Resource Inspector, saved-root hover, completion, navigation |
| Checker-owned source facts | Checked hover/navigation/completion/signature facts for declarations, module paths, type annotations, docs, builtins, operators, enum paths, and saved roots | Source tooling |
| Checked saved places and effects | Lowered write plans, durable writes, typed runtime values, transitive effects, and durable scope not covered by current session/data APIs | Data tools, DAP, run/debug UI |
| Production run protocol facts | Durable-scope/effect facts, transaction facts, runtime generation facts, and attach/serve execution boundaries | MCP `mw_run` production graduation, DAP launch/evaluate |
| Debugger facts | Frame scopes, production expression evaluation, durable watch targets, store-generation facts, typed path segments, and serve/attach boundaries | DAP |
| Store admin facts | Carried snapshot metadata is not a source/store compatibility verdict. Durable data views still need drift witnesses, typed repair facts, watch/refresh contracts, serve/attach boundaries, and stable production integrity DTOs beyond current saved-data read, walk, count, and integrity sampling APIs. | Saved Resource Inspector, MCP data tools, data integrity |
| Typed protocol DTOs | Catalog-epoch-bound DTOs for resources, stores, diagnostics, integrity, repair, debugger values, and admin views | MCP, DAP, VS Code data views |

## Removed Or Blocked Surfaces

The following prior surfaces have no production authority and must not reappear
without a new Marrow fact and a review-gated lane:

- Local durable-data classifiers and private store-key readers.
- Editor or transport options that opt into raw durable-data inspection.
- Launch/run argument paths that construct runtime values without Marrow's typed entry protocol admission.
- Live saved-data hover and live record-count hints.
- TypeScript child-path derivation for the Saved Resource Inspector.
- Durable watch parsing in DAP.
- VS Code launch snippets that seed invalid placeholder entries or raw positional
  args.
- DAP hit-condition/logpoint breakpoint expressions and durable watch parsing.
- MCP all-resource schema listing without a named resource or paged Marrow DTO.
- Source-spelling identities for saved roots, resource members, indexes, enum
  members, aliases, or physical store keys.

## Handoff Rule

When a lane discovers a missing fact, it records the exact Marrow fact needed and
returns an unavailable contract or deletes the surface. It does not add local
classifiers, compatibility switches, untyped path protocols, message parsing, or
alternate semantic models in `marrow-lsp`.
