# Marrow-LSP Production Readiness Roadmap

## Current Release State

The current valid release claim is:

```text
production-safe integration with blocked non-production surfaces
```

`marrow-lsp` is integrated over the current public Marrow APIs. LSP, MCP, DAP,
and VS Code surfaces are release-safe only under the verdicts recorded in
[`lsp-fact-consumption-ledger.md`](lsp-fact-consumption-ledger.md): `ready`,
`presentation-only`, `blocked-on-marrow`, or `deleted`.

Do not claim `fully production-ready` until the ledger has no production surface
left as `presentation-only` or `blocked-on-marrow`.

## Release Contract

- Marrow owns syntax, checking, typed identities, catalog facts, store facts,
  runtime facts, debugger facts, and durable data semantics.
- `marrow-lsp-core` consumes canonical Marrow facts and produces
  transport-neutral presentation data.
- `marrow-lsp`, `marrow-mcp`, `marrow-dap`, and the VS Code extension stay thin
  transports and views.
- Missing Marrow facts produce a blocked or deleted surface, not a local
  classifier, fallback path, compatibility shim, TypeScript inference path, or
  source-spelling identity.
- Presentation-only surfaces must be bounded, tested, and explicit about their
  non-production contract.
- The hosted GitHub release workflow is not release evidence. Release evidence
  comes from fresh local gates and package smoke checks.

## Required Fresh Evidence

Before any release claim, run the full bar from a clean checkout:

```sh
git diff --check
CARGO_TARGET_DIR=<outside-repo-target-dir>/<lane> cargo fmt --manifest-path <worktree>/Cargo.toml --all -- --check
CARGO_TARGET_DIR=<outside-repo-target-dir>/<lane> cargo build --manifest-path <worktree>/Cargo.toml --workspace --all-features
CARGO_TARGET_DIR=<outside-repo-target-dir>/<lane> cargo test --manifest-path <worktree>/Cargo.toml --workspace --all-features
CARGO_TARGET_DIR=<outside-repo-target-dir>/<lane> cargo clippy --manifest-path <worktree>/Cargo.toml --workspace --all-targets --all-features -- -D warnings
! rg -n '\bunsafe\b' crates editors/vscode/src
npm --prefix editors/vscode ci
npm --prefix editors/vscode run check
npm --prefix editors/vscode run compile
npm --prefix editors/vscode audit --audit-level=moderate
```

Every Cargo command must use an explicit outside-repository `CARGO_TARGET_DIR`.
Do not share target directories across concurrent lanes, and delete the lane
target directory immediately after the relevant gate. A clean hosted workflow
cannot replace these local gates.

## Graduation Work

The remaining work is not LSP-side hardening debt unless the ledger says Marrow
already exposes the needed fact. Semantic items below must either consume a
canonical Marrow API or remain blocked; non-semantic editor and packaging rows
must explicitly avoid language, catalog, runtime, or storage authority.

| Area | Graduation requirement |
| --- | --- |
| Source intelligence | Source navigation, source-only rename, completion, signature help, semantic tokens, document/workspace symbols, module-path facts, source docs, operators, source hover, and catalog-backed definition/references are production-semantic through Marrow facts in the ledger. Saved-data-backed editor rename application remains absent until Marrow exposes an editor application target and insertion contract. |
| Data views | Read-only saved-root, child-page, and value-read inspection is a production data-view contract through Marrow typed saved-data DTOs, data-view boundaries, watch targets, and checked-program admission. Standalone stopped-frame durable reads are production-semantic through Marrow debug-data facts and boundaries. Integrity, repair, and served-process data views stay absent until Marrow exposes their production facts. |
| MCP run | Served-process boundaries over the existing Marrow run/test sessions. Current run mode stays sandboxed over a fresh in-memory store without opening, reading, copying, or writing the configured project store, and returns Marrow run result/error DTOs plus versioned analysis generation, entry descriptor, footprint, cost-shape, and store-open-mode facts after entry admission. |
| DAP | Standalone isolated launch is production-semantic through Marrow project sessions, entry admission, runtime stop-point facts, debug-expression facts, execution boundaries, serve-boundary metadata with `processControl.kind = "not_exposed"`, local debug facts, and stopped-frame durable-data boundaries. Served-process attach/debug launch remains absent until Marrow exposes served-process control boundaries. |
| VS Code | Editor commands, snippets, grammar, activation, and launcher plumbing expose only ready contracts. Future semantic inputs stay absent from schemas and snippets until Marrow exposes the required facts; exposed standalone debug entry/arg inputs remain DAP-owned schema only. |

## Maintenance Rules

- Update the fact ledger in the same change as any surface graduation, deletion,
  or newly discovered blocker.
- Delete completed implementation plans and lane history from tracked docs.
- Keep future roadmap items concrete: name the Marrow fact, LSP surface, test
  harness, and absence scan.
- Do not add compatibility flags, prototype bridges, local semantic classifiers,
  or message-prose assertions to keep old behavior alive.
