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
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/<lane> cargo fmt --manifest-path /Users/scottwilliams/Dev/<worktree>/Cargo.toml --all -- --check
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/<lane> cargo build --manifest-path /Users/scottwilliams/Dev/<worktree>/Cargo.toml --workspace --all-features
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/<lane> cargo test --manifest-path /Users/scottwilliams/Dev/<worktree>/Cargo.toml --workspace --all-features
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/<lane> cargo clippy --manifest-path /Users/scottwilliams/Dev/<worktree>/Cargo.toml --workspace --all-targets --all-features -- -D warnings
! rg -n '\bunsafe\b' crates editors/vscode/src
npm --prefix editors/vscode ci
npm --prefix editors/vscode run check
npm --prefix editors/vscode run compile
npm --prefix editors/vscode audit --audit-level=moderate
```

Every command must use an explicit outside-repository `CARGO_TARGET_DIR`. A
clean hosted workflow cannot replace these local gates.

## Graduation Work

The remaining work is not LSP-side hardening debt unless the ledger says Marrow
already exposes the needed fact. Each item below must either consume a canonical
Marrow API or remain blocked.

| Area | Graduation requirement |
| --- | --- |
| Source intelligence | Checker-owned Marrow facts for hover, navigation, completion, signature help, module paths, type annotations, docs, builtins, operators, enum paths, saved roots, and rename safety. |
| Data views | Versioned catalog/store generations, watch/refresh semantics, integrity and repair facts, production serve/attach boundaries, and debugger/runtime data-view contracts. Current saved-data path DTOs, child paging, and value previews are Marrow-owned but remain debug/admin presentation surfaces until those contracts exist. |
| MCP run | A Marrow run/session mode that keeps checked execution over a fresh in-memory store without opening, reading, copying, or writing the configured project store, plus durable-scope/effect facts, transaction facts, runtime generation facts, and serve/attach boundaries. |
| DAP | Frame scopes, production expression evaluation, durable watch targets, store-generation facts, breakpoint expression facts for conditions, hit conditions, and logpoints, plus serve/attach boundaries. Local value expansion already consumes Marrow runtime debug facts. |
| VS Code | Editor commands and snippets may expose only ready or presentation-only contracts. Blocked semantic inputs stay absent from schemas and snippets; exposed debug entry/arg inputs remain DAP-owned schema only. |

## Maintenance Rules

- Update the fact ledger in the same change as any surface graduation, deletion,
  or newly discovered blocker.
- Delete completed implementation plans and lane history from tracked docs.
- Keep future roadmap items concrete: name the Marrow fact, LSP surface, test
  harness, and absence scan.
- Do not add compatibility flags, prototype bridges, local semantic classifiers,
  or message-prose assertions to keep old behavior alive.
