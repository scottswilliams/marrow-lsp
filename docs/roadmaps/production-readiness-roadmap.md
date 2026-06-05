# Marrow-LSP Production Readiness Hardening Roadmap

This roadmap is the active execution plan for hardening `marrow-lsp` into a
production-quality downstream toolchain over Marrow. It supersedes stale
compile-restoration and baseline trackers. The feature roadmap remains useful
directional context, and the fact ledger remains the surface contract, but this
file controls hardening order and completion claims.

## Goal

Make `marrow-lsp` a production-quality downstream toolchain over Marrow, with no
prototype code paths, duplicated Marrow semantics, compatibility glue, stale
docs, weak Rust, or user-facing surface that presents non-production behavior as
production.

If any production surface remains presentation-only or blocked on Marrow facts,
the only valid completion claim is:

```text
production-safe integration with blocked non-production surfaces
```

A `fully production-ready` claim is valid only when every LSP, MCP, DAP, and VS
Code surface is production-ready, deleted, or explicitly non-semantic
presentation that cannot be mistaken for semantic authority.

## Operating Rules

- Marrow owns syntax, checking, typed identities, catalog facts, store facts,
  runtime facts, debugger facts, and durable data semantics.
- `marrow-lsp-core` consumes canonical Marrow facts and produces
  transport-neutral presentation data.
- `marrow-lsp`, `marrow-mcp`, `marrow-dap`, and the VS Code extension stay thin
  transports and views.
- Missing Marrow facts produce a blocked or deleted surface, not a local
  classifier, fallback path, compatibility shim, TypeScript inference path, or
  source-spelling identity.
- Work happens in file-disjoint lanes where possible. Each substantial lane uses
  an isolated worktree directly under `/Users/scottwilliams/Dev` and its own
  `CARGO_TARGET_DIR` under `/Users/scottwilliams/Dev/.cargo-targets`.
- Every behavior change starts from a failing focused check. Every integration
  requires two read-only reviews: one soundness review and one idiom/spec review.

## Lane Integration Bar

Each lane records exact base and head commits, changed files, focused checks,
full gates, reviewer verdicts, and absence scans. Cargo commands must spell the
lane target dir and manifest path explicitly.

Required full gates before pushing a lane:

```sh
git diff --check
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/<lane> cargo fmt --manifest-path /Users/scottwilliams/Dev/<lane-worktree>/Cargo.toml --all -- --check
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/<lane> cargo build --manifest-path /Users/scottwilliams/Dev/<lane-worktree>/Cargo.toml --workspace --all-features
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/<lane> cargo test --manifest-path /Users/scottwilliams/Dev/<lane-worktree>/Cargo.toml --workspace --all-features
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/<lane> cargo clippy --manifest-path /Users/scottwilliams/Dev/<lane-worktree>/Cargo.toml --workspace --all-targets --all-features -- -D warnings
! rg -n '\bunsafe\b' crates editors/vscode/src
npm --prefix editors/vscode ci
npm --prefix editors/vscode run check
npm --prefix editors/vscode run compile
```

Rust-only or docs-only lanes may run a justified subset locally, but the
integration report must state why omitted gates were not relevant. The final
evidence lane runs the full bar.

## Lane 0: Coordination Cleanup

Goal: make tracked docs consistent before source hardening resumes.

Required outcomes:

- The active hardening roadmap is tracked.
- Stale red-baseline and completed-history documents are deleted or clearly
  moved out of the active execution path.
- Roadmap files do not point to missing files.
- The fact ledger remains the surface-status authority until the product-surface
  truth lane refreshes it.

Review focus:

- No stale commit-specific baseline evidence is presented as current.
- No roadmap authorizes LSP or VS Code behavior contrary to the Marrow-first
  direction.

## Lane 1: Product-Surface Truth

Goal: audit every LSP, MCP, DAP, and VS Code surface and ensure each surface has
one honest contract.

For each surface, choose exactly one ledger verdict:

- `ready`: production-ready, backed by canonical Marrow facts, and tested through a
  stable protocol contract.
- `presentation-only`: clearly non-semantic, bounded, tested, and impossible to
  confuse with semantic authority.
- `blocked-on-marrow`: needs a Marrow fact and has no executable prototype
  path beyond a stable unavailable result.
- `deleted`: removed from commands, protocol handlers, package manifests,
  snippets, docs, and tests.

Required audit targets:

- LSP capabilities, hover, navigation, semantic tokens, completion, signature
  help, formatting, diagnostics, symbols, rename, references, and workspace
  analysis.
- MCP tools, especially `mw_run`, `mw_complete`, `mw_resource_schema`,
  `mw_saved_roots`, saved-data tools, and schema/completion helpers.
- DAP launch, debug entry selection, breakpoints, variables, stack frames,
  output, watches, expression evaluation, and launch argument handling.
- VS Code commands, views, debug snippets, data explorer, data integrity command,
  package contributions, grammar, and extension activation behavior.

Required outputs:

- A refreshed surface verdict table in
  `docs/roadmaps/lsp-fact-consumption-ledger.md`.
- Tests or source scans proving deleted or blocked surfaces cannot still be
  invoked through stale commands, snippets, handlers, or TypeScript views.
- A blocker table naming each Marrow fact required to graduate blocked features.

## Lane 2: Rust Code-Shape Hardening

Goal: apply the Rust quality rules from `/Users/scottwilliams/Dev/AGENTS.md` to
the main presentation and transport modules.

Audit and split, shrink, or justify:

- `crates/marrow-lsp-core/src/hover.rs`
- `crates/marrow-lsp-core/src/semantic_tokens.rs`
- `crates/marrow-lsp-core/src/navigation.rs`
- `crates/marrow-lsp-core/src/signature_help.rs`
- `crates/marrow-lsp-core/src/completion.rs`
- `crates/marrow-lsp-core/src/mcp.rs`
- `crates/marrow-lsp-core/src/data_explorer.rs`
- `crates/marrow-lsp-core/src/data_integrity.rs`
- `crates/marrow-lsp-core/src/store.rs`
- `crates/marrow-lsp/src/backend.rs`
- `crates/marrow-lsp/src/main.rs`
- `crates/marrow-mcp/src/main.rs`
- `crates/marrow-mcp/src/server.rs`
- `crates/marrow-dap/src/debugger.rs`
- `crates/marrow-dap/src/main.rs`
- `crates/marrow-dap/src/project.rs`
- `crates/marrow-dap/src/protocol.rs`
- `crates/marrow-dap/src/run.rs`
- `crates/marrow-dap/src/session.rs`
- `crates/marrow-dap/src/step.rs`
- `crates/marrow-dap/src/variables.rs`
- `crates/marrow-lsp/tests/smoke.rs`

Required outcomes:

- Broad dispatcher functions are split into focused helpers or modules.
- Duplicate classifiers are removed or blocked under Lane 3 rules.
- Comments explain durable rationale only.
- Any intentionally large file has a reviewer-approved boundary plan and an
  explicit reason it should not be split yet.

## Lane 3: Canonical-Fact Ownership

Goal: eliminate local semantic ownership.

Find every local token, source, path, runtime, store, debugger, completion,
hover, navigation, signature, or role classifier that decides meaning. For each
one:

- replace it with an existing Marrow fact;
- move the fact request upstream to Marrow and block or delete the surface; or
- prove it is lexical presentation only and cannot become semantic authority.

Disallowed survivors:

- local durable identity logic;
- local saved-path semantics;
- source spelling as stable identity;
- duplicate language classifiers outside `marrow-syntax` or `marrow-check`;
- TypeScript inference of data, catalog, or language meaning.

## Lane 4: Test-Quality Hardening

Goal: make tests assert typed contracts instead of prose or prototype behavior.

Required outcomes:

- Replace diagnostic prose matching with diagnostic codes, spans, severities,
  typed payloads, or protocol fields.
- Replace output-message fragments with status enums, stable unavailable codes,
  capability flags, or typed contract objects.
- Keep prose assertions only where the test is explicitly a rendering test.
- Delete fixtures and tests whose only purpose is preserving obsolete prototype
  behavior.

## Lane 5: VS Code And Package Hardening

Goal: make the extension package honest and shippable for the narrowed product
state.

Required checks:

```sh
npm --prefix editors/vscode ci
npm --prefix editors/vscode run check
npm --prefix editors/vscode run compile
npm --prefix editors/vscode audit --audit-level=moderate
```

Required outcomes:

- Fix the npm audit failure, or record an explicit reviewed dependency exception
  with package, advisory, severity, exploitability, mitigation, and owner.
- Run package or bundle smoke checks if the repository provides them.
- Remove production-looking commands, snippets, debug configurations, or views
  for blocked semantics.
- Ensure every remaining VS Code command has an honest, bounded contract.

## Lane 6: File-By-File Style Audit

Goal: prove the AGENTS.md quality rules are followed across the repository.

Required output:

- A tracked audit plan that lists every production Rust and TypeScript file.
- For each file, a function-by-function verdict covering ownership, semantics,
  API shape, test quality, comments, prototype terms, local classifiers, raw
  path usage, oversized dispatch, and duplicate logic.
- A fix lane or blocker for every failed verdict.

This lane is not a place to defer known lane-local cleanup. Earlier lanes must
fix issues they own before integration. Lane 6 verifies no unowned residue
remains.

## Lane 7: Final Production-Readiness Evidence

Goal: produce the only allowed final claim from fresh evidence.

Required evidence:

- clean local and origin integration state;
- exact base and head commits;
- changed files;
- surface verdict table for every LSP, MCP, DAP, and VS Code capability;
- focused gates;
- full Rust gates;
- full TypeScript and package gates;
- npm audit result;
- unsafe scan;
- stale-doc scan;
- prototype, legacy, fallback, compatibility, and raw-path scan;
- oversized-file verdicts;
- prose-assertion verdicts;
- soundness reviewer verdict;
- idiom/spec reviewer verdict;
- fixes for every review finding and re-review verdict.

The final report must either claim `fully production-ready` with no
presentation-only or blocked production surfaces remaining, or claim
`production-safe integration with blocked non-production surfaces` and list the
Marrow facts required to graduate each blocked feature.
