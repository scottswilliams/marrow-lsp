# Marrow V0.1 Parallel Orchestration

This roadmap is historical coordination context for the Marrow project lead and
the `marrow-lsp` orchestrator during the prototype-to-v0.1 rewrite. Active
`marrow-lsp` hardening now runs from
[`production-readiness-roadmap.md`](production-readiness-roadmap.md). The rule
remains simple: Marrow owns semantics; `marrow-lsp` owns presentation,
transports, and editor workflows over canonical Marrow facts.

## Parallel Tracks

### Marrow Project Lead

The project lead drives upstream foundation lanes in `/Users/scottwilliams/Dev/marrow`:

1. Finish prototype rejection and docs alignment.
   Canonical docs must stop presenting `@id`, broad `merge`, `lock`, raw paths,
   source-order enum ordinals, saved `inout`, raw protocols, and migration
   scripts as v0.1 production commitments.
2. Land the shared v0.1 fixture skeleton.
   The same source fixture should exercise checker, runtime, store, CLI/LSP,
   evolution, and backup as each layer becomes real.
3. Build the checked facts nucleus.
   `marrow-check` must expose typed IDs, durable places, effects, diagnostics,
   symbol definitions, and typed snapshots with version/digest semantics.
4. Split resource/store/catalog identity.
   Resource identity, store identity, field/layer/index identity, enum member
   identity, and catalog epochs must be facts, not source-name conventions.
5. Publish an LSP impact note for every semantic lane.
   Each lane summary must say whether LSP behavior is unchanged, updated through
   existing facts, or blocked on a new fact/API.

The project lead should not ask `marrow-lsp` to compensate for missing upstream
facts. If the LSP needs a semantic fact, the project lead either lands the
canonical Marrow API first or records the LSP work as blocked.

### LSP Orchestrator

The LSP orchestrator can safely run file-disjoint lanes in `/Users/scottwilliams/Dev/marrow-lsp`
while Marrow foundation work continues:

1. Fact-consumption audit.
   Map every LSP feature to the Marrow fact/API it consumes. Mark each feature
   as `ready`, `blocked-on-marrow`, or `presentation-only`. This lane owns the
   shared ledger file; concurrent lanes may read it but must not edit it.
2. Transport/core boundary cleanup.
   Keep `marrow-lsp`, `marrow-mcp`, `marrow-dap`, and the VS Code extension thin.
   Move shared presentation helpers into `marrow-lsp-core`; delete transport-local
   semantic classifiers.
3. Fixture harness preparation.
   Add or refine editor/tooling fixtures that load real Marrow projects and
   assert diagnostics, hover, navigation, semantic tokens, completion, MCP, and
   DAP behavior without duplicating the compiler.
4. Presentation-only UX improvements.
   Improve Markdown rendering, bounded summaries, error display, command wiring,
   VS Code packaging checks, and transport smoke tests when they use facts that
   already exist.
5. Capability-gated adapters.
   Where upstream facts are missing, write capability gates and lane-local review
   notes that name the missing Marrow fact. Do not infer the fact locally. Fold
   those notes into the shared ledger only in a later serialized docs lane.

The LSP orchestrator should stop any lane that requires new language,
catalog, storage, runtime, or data-evolution semantics. That work returns to the
Marrow project lead as an upstream lane.

## Do Not Parallelize

Do not run these as LSP-only work:

- parsing Marrow syntax outside `marrow-syntax`;
- reclassifying types, resources, stores, paths, enum members, or write effects;
- interpreting raw saved paths as stable editor identity;
- adding TypeScript logic for language semantics;
- preserving prototype behavior for editor convenience;
- adding new dependencies to patch over missing Marrow facts.

## Historical Handoff Shape

This handoff shape remains useful reference material. Active lane selection,
scope, gates, and completion claims are controlled by
[`production-readiness-roadmap.md`](production-readiness-roadmap.md).

Every Marrow semantic lane can produce this short handoff for the LSP
orchestrator:

```text
LSP impact:
- status: unchanged | update-existing-facts | blocked-on-new-fact
- facts added/changed:
- diagnostics changed:
- docs changed:
- fixtures to reuse:
- LSP files likely affected:
```

Every LSP lane can produce this short handoff for the Marrow project lead:

```text
Marrow dependency:
- status: none | uses-existing-fact | blocked-on-missing-fact
- canonical API consumed:
- missing upstream fact, if any:
- no duplicated semantics proof:
```
