# Marrow V0.1 Parallel Orchestration

This roadmap coordinates the Marrow project lead and the `marrow-lsp`
orchestrator during the prototype-to-v0.1 rewrite. The rule is simple:
Marrow owns semantics; `marrow-lsp` owns presentation, transports, and editor
workflows over canonical Marrow facts.

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
   symbol definitions, and query-native snapshots with version/digest semantics.
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

## Synchronization Contract

Every Marrow semantic lane produces this short handoff for the LSP orchestrator:

```text
LSP impact:
- status: unchanged | update-existing-facts | blocked-on-new-fact
- facts added/changed:
- diagnostics changed:
- docs changed:
- fixtures to reuse:
- LSP files likely affected:
```

Every LSP lane produces this short handoff for the Marrow project lead:

```text
Marrow dependency:
- status: none | uses-existing-fact | blocked-on-missing-fact
- canonical API consumed:
- missing upstream fact, if any:
- no duplicated semantics proof:
```

## First Parallel Wave

Run these lanes together only with the concrete write scopes below. If a lane
needs a file owned by another active lane, pause it and serialize.

| Lane | Owner | Repository | Write Scope | Output |
| --- | --- | --- | --- | --- |
| Prototype rejection | Marrow project lead | `marrow` | docs/checker diagnostics | Canonical v0.1 docs and rejection diagnostics |
| Fact-consumption audit | LSP orchestrator | `marrow-lsp` | `docs/roadmaps/lsp-fact-consumption-ledger.md` | Feature-to-fact matrix and blocked-fact ledger |
| Transport boundary cleanup | LSP orchestrator | `marrow-lsp` | `crates/marrow-lsp/src/**`, `crates/marrow-mcp/src/**`, `crates/marrow-dap/src/**`, `editors/vscode/src/**`; no fixtures, core, or shared ledger writes | Deleted duplicated presentation/semantic drift plus lane-local blocker notes |
| Fixture harness preparation | LSP orchestrator | `marrow-lsp` | `fixtures/**`, `crates/*/tests/**`, `editors/vscode/scripts/**`; no transport source, core source, or shared ledger writes | Source-driven tooling fixtures plus lane-local blocker notes |

Only the Marrow project lead touches language docs or upstream semantic APIs in
this wave. The LSP lanes may read `/Users/scottwilliams/Dev/marrow`, but treat
it as read-only unless the project lead explicitly opens a Marrow lane.

## Executor Prompt

Use this prompt for the `marrow-lsp` orchestrator:

```text
You are the marrow-lsp orchestrator working in /Users/scottwilliams/Dev/marrow-lsp.
Coordinate in parallel with the Marrow project lead, who owns upstream semantics
in /Users/scottwilliams/Dev/marrow.

Objective:
Advance marrow-lsp toward the Marrow v0.1 rewrite without inventing or preserving
prototype semantics. Build editor/tooling infrastructure that consumes canonical
Marrow facts, deletes duplicated classifiers, and records blockers when Marrow
does not yet expose a fact.

Read first:
- /Users/scottwilliams/Dev/AGENTS.md
- /Users/scottwilliams/Dev/marrow-lsp/AGENTS.md
- /Users/scottwilliams/Dev/marrow-lsp/docs/roadmaps/v01-parallel-orchestration.md
- /Users/scottwilliams/Dev/marrow/docs/roadmap/prototype-to-v1-execution-plan.md
- relevant accepted ADR files in /Users/scottwilliams/Dev/marrow-decisions

Operating rules:
- Use isolated worktrees directly under /Users/scottwilliams/Dev/ so the path
  dependency ../marrow still resolves. Retire each worktree immediately after
  integration.
- Give every worktree its own external Rust build-output directory. VS Code
  scripts may write ignored editors/vscode/out inside the isolated worktree;
  delete editors/vscode/out before integration.
- For each substantial lane: one TDD build agent, then two read-only senior
  reviews: soundness and idiom/spec.
- Do not edit /Users/scottwilliams/Dev/marrow from an LSP lane. If a feature
  needs a new Marrow fact/API, stop and record a blocked-on-marrow handoff.
- No TypeScript parsing or semantic inference. The VS Code extension is a thin
  launcher/view layer.
- No new dependencies without explicit approval.

First wave:
1. Fact-consumption audit lane.
   Write docs/roadmaps/lsp-fact-consumption-ledger.md. Map diagnostics, hover,
   navigation, semantic tokens, completion, signature help, formatting, data
   explorer, MCP, and DAP to the Marrow facts they consume. Mark each item
   ready, presentation-only, or blocked-on-marrow. No code changes.
2. Transport boundary cleanup lane.
   Search for duplicated semantic classifiers in editors/vscode, crates/marrow-lsp,
   crates/marrow-mcp, and crates/marrow-dap. Delete or move only presentation
   helpers that are already fact-backed by marrow-lsp-core. Leave semantic gaps
   as lane-local blocker notes; do not edit fixtures, core source, tests, or the
   shared ledger in this lane.
3. Fixture harness preparation lane.
   Add source-driven tooling fixtures and focused tests that assert current
   facts without freezing prototype semantics. Tests for future Marrow facts
   must be recorded in lane-local blocker notes rather than ignored silently.
   Do not edit transport source, core source, or the shared ledger in this lane.

Integration bar:
- git diff --check origin/main..HEAD
- cargo fmt --manifest-path <worktree>/Cargo.toml --all --check
- CARGO_TARGET_DIR=<target> cargo build --manifest-path <worktree>/Cargo.toml --workspace --all-features
- CARGO_TARGET_DIR=<target> cargo test --manifest-path <worktree>/Cargo.toml --workspace --all-features
- CARGO_TARGET_DIR=<target> cargo clippy --manifest-path <worktree>/Cargo.toml --workspace --all-targets --all-features -- -D warnings
- npm --prefix <worktree>/editors/vscode run check
- npm --prefix <worktree>/editors/vscode run compile
- rm -rf <worktree>/editors/vscode/out before integration
- ! rg -n '\bunsafe\b' <worktree>/crates
- final read-only review of origin/main..HEAD before push

Integration sequence:
- fetch and record origin/main;
- rebase the reviewed lane onto that exact commit;
- run the lane focused gate;
- update the live main worktree to the same origin/main;
- cherry-pick the reviewed commit with -x;
- run the full integration bar from the live main worktree with a fresh target;
- request a final read-only review of origin/main..HEAD;
- fetch again and push only if origin/main has not moved.

Report after each lane:
- files changed
- Marrow dependency status
- tests/gates run with fresh output
- reviewer verdicts
- blockers added to the shared ledger or lane-local notes
- worktree retired or reason it remains
```
