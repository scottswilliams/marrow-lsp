# Marrow LSP Release Readiness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring `marrow-lsp` to release-level featurefulness, data quality, and idiomatic Rust quality over current public Marrow APIs and explicit stable release boundaries.

**Architecture:** Keep `marrow-lsp-core` as the only semantic brain and keep LSP, MCP, DAP, and VS Code as thin transports or views. Every lane consumes canonical Marrow facts where they exist, records a precise upstream blocker where they do not, and deletes or blocks any prototype path rather than preserving compatibility glue.

**Tech Stack:** Rust 1.89, Cargo workspace crates, `tower-lsp`, Marrow path crates from `../marrow`, VS Code TypeScript extension, npm package scripts, MCP stdio, DAP stdio.

---

## Execution Rules

- Work from short-lived worktrees directly under `/Users/scottwilliams/Dev` so `../marrow` path dependencies resolve.
- Use a lane-specific `CARGO_TARGET_DIR` under `/Users/scottwilliams/Dev/.cargo-targets` in every Cargo command.
- Write or identify a failing check before behavior changes. For dependency or packaging hardening, the failing gate is the package/audit command.
- Keep write sets disjoint for concurrent lanes. Do not run two workers against the same Rust module or manifest.
- Each lane gets parallel read-only soundness and idiom/spec reviews. The lane may integrate only after every finding is fixed and both reviewers return a clean PASS re-review.
- A lane is done only with clean status, changed-file list, fresh focused checks, fresh required gates, PASS reviewer verdicts, and an absence scan for the old pattern it removed.

## Lane Scheduling

The tasks below are not all parallel-safe.

- Task 1 may run first by itself because it writes docs and establishes the execution contract.
- Task 2 may run in parallel with Task 1 review because it writes only VS Code package manifests.
- Task 3 and Task 4 both touch `crates/marrow-lsp-core/src/mcp.rs`, so run them sequentially.
- Task 5 may not overlap Task 2 or Task 7 if it edits `editors/vscode/package.json`.
- Task 7 runs after Task 2 and after any VS Code schema changes from Task 5.
- Task 8 is final-only and runs after all implementation lanes are integrated.

Before any behavior lane starts, its starter prompt must record the exact lane name, worktree path, target dir, failing test name, harness layer, fixture, oracle, and deletion or absence target. If those details are not known, the first step of that lane is a read-only planning pass; no production code changes are allowed during that pass.

## Baseline Gates

Every non-doc lane must run the concrete lane commands listed in its task,
plus the matching full gate before integration: whitespace check, Rust format,
Rust build, Rust tests, Rust clippy with `-D warnings`, unsafe absence scan,
VS Code install, VS Code check, VS Code compile, and npm audit. The final
evidence lane lists the full command packet with fixed paths.

Docs-only lanes may run `git diff --check` plus targeted link/status scans, but their integration note must say why Rust, npm, and audit gates were not relevant.

### Task 1: Rebase The Roadmap And Fact Ledger

**Files:**
- Modify: `docs/roadmaps/production-readiness-roadmap.md`
- Modify: `docs/roadmaps/lsp-fact-consumption-ledger.md`
- Create: `docs/superpowers/plans/2026-06-17-lsp-release-readiness.md`

- [ ] **Step 1: Record current Marrow API adoption candidates**

Add a ledger section naming current public Marrow APIs that change old blocker status:

```text
marrow_run::ProjectSession, ProjectOpen, SessionEntry, CheckedEntryCall
marrow_check::CheckedProgram entry and digest facts
marrow_check::tooling data query, data children, data walk, rendering, counts, integrity APIs
marrow_store::CommitMetadata and read-only snapshot/catalog APIs
```

- [ ] **Step 2: Reclassify stale blockers without changing code**

For each stale blocker, keep the current surface safe but mark the blocker as LSP adoption work when Marrow already exposes enough API. Do not mark a feature `ready` until the implementation consumes the API and tests prove it.

- [ ] **Step 3: Verify the docs-only lane**

Run:

```sh
git diff --check
rg -n 'blocked-on-marrow|ProjectSession|data_children|resolve_data_query|walk_data|sample_integrity' docs
git status --short --branch
```

Expected: whitespace check passes; scan shows the new API rebase language; status lists only the intended docs changes.

### Task 2: Fix VS Code Package Audit

**Files:**
- Modify: `editors/vscode/package.json`
- Modify: `editors/vscode/package-lock.json`

- [ ] **Step 1: Verify the current audit failure**

Run:

```sh
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-package-audit/editors/vscode ci
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-package-audit/editors/vscode audit --audit-level=moderate
```

Expected: audit fails on direct `esbuild` advisories below `0.28.1`.

- [ ] **Step 2: Upgrade only the vulnerable direct dev dependency**

Change the direct `esbuild` dependency to the smallest safe version accepted by npm audit. Do not add a new bundler or unrelated packages.

- [ ] **Step 3: Verify package hardening**

Run:

```sh
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-package-audit/editors/vscode install
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-package-audit/editors/vscode run check
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-package-audit/editors/vscode run compile
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-package-audit/editors/vscode audit --audit-level=moderate
git diff --check
```

Expected: all commands exit 0 and only `package.json` plus `package-lock.json` changed.

### Task 3: Gate MCP Run Against Stable Session Facts

**Files:**
- Modify: `crates/marrow-lsp-core/src/mcp.rs`
- Modify: `crates/marrow-lsp-core/src/mcp/tests.rs`
- Conditional modify: `crates/marrow-mcp/src/server.rs` only when the typed MCP envelope changes; otherwise this file is outside the lane write set.

- [ ] **Step 1: Verify the MCP run session boundary before code changes**

In lane `marrow-lsp-mcp-session`, inspect Marrow's session API before writing production code. MCP `mw_run` may adopt a session path only if Marrow exposes a mode that forces checked zero-argument execution over a fresh in-memory store without opening, reading, copying, or writing the configured project store. Entry identity must also have a stable canonical DTO before the explicit `entry` string can graduate beyond presentation-only.

- [ ] **Step 2: Stop if the stable boundary is absent**

If the stable boundary is absent, do not add a passing test that merely blesses current behavior. Record the blocker in the fact ledger and leave MCP run code unchanged.

Required blocker evidence:

- whether entry override remains string-based;
- whether argument input remains text-only rather than typed protocol DTOs;
- whether session open can open, read, copy, or write the configured project store;
- which exact Marrow API would unblock MCP without weakening its sandbox.

- [ ] **Step 3: Write a failing MCP test only when safe adoption exists**

If Marrow exposes the required stable boundary, add a focused test named `run_entry_override_uses_session_path` in `mcp/tests.rs`. The test must run a project through the safe session path and assert typed result fields, sandbox metadata, and the unchanged presentation-only entry contract.

- [ ] **Step 4: Replace manual run setup only where the API covers the contract**

Use Marrow session APIs only where they preserve fresh in-memory execution, the locked host, no project-store access, presentation-only entry strings, and blocked non-empty args unless typed run argument facts exist.

- [ ] **Step 5: Verify MCP run behavior**

If implementation proceeds, run:

```sh
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-mcp-session cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-mcp-session/Cargo.toml -p marrow-lsp-core run_entry_override_uses_session_path
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-mcp-session cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-mcp-session/Cargo.toml -p marrow-lsp-core mcp::tests
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-mcp-session cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-mcp-session/Cargo.toml --workspace --all-features
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-mcp-session cargo clippy --manifest-path /Users/scottwilliams/Dev/marrow-lsp-mcp-session/Cargo.toml --workspace --all-targets --all-features -- -D warnings
```

If the lane is blocked before code changes, run:

```sh
git diff --check
rg -n 'MCP `mw_run`|Forced-memory run session|fresh in-memory store' docs/roadmaps/lsp-fact-consumption-ledger.md docs/superpowers/plans/2026-06-17-lsp-release-readiness.md
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-mcp-session cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-mcp-session/Cargo.toml -p marrow-lsp-core mcp::tests
```

### Task 4: Adopt Marrow Data Query APIs In Core

**Files:**
- Modify: `crates/marrow-lsp-core/src/data_explorer.rs`
- Modify: `crates/marrow-lsp-core/src/store.rs`
- Modify: `crates/marrow-lsp-core/src/mcp.rs`
- Test: `crates/marrow-lsp-core/src/data_explorer.rs` module tests
- Test: `crates/marrow-lsp-core/src/mcp/tests.rs`

- [ ] **Step 1: Write failing tests for paged data children and typed query errors**

In lane `marrow-lsp-data-query`, add tests named `data_children_returns_paged_typed_segments` and `data_query_errors_are_typed` next to the core data-explorer or MCP data tests. The tests must exercise `marrow_check::tooling` query APIs through the LSP core layer, not TypeScript path derivation.

- [ ] **Step 2: Run the focused tests and confirm current root-only behavior fails**

Run:

```sh
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-data-query cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-data-query/Cargo.toml -p marrow-lsp-core data_children_returns_paged_typed_segments
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-data-query cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-data-query/Cargo.toml -p marrow-lsp-core data_query_errors_are_typed
```

- [ ] **Step 3: Implement a shared transport-neutral data query DTO**

The DTO must carry typed root/member/key segments, paging cursor data when Marrow returns it, store/catalog generation metadata when Marrow returns it, and typed error codes. It must not materialize unbounded records.

- [ ] **Step 4: Wire MCP and custom LSP data surfaces through the shared DTO**

MCP and VS Code-facing custom requests must consume the same core data query layer. TypeScript remains a view.

- [ ] **Step 5: Verify data surfaces**

Run:

```sh
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-data-query cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-data-query/Cargo.toml -p marrow-lsp-core data_children_returns_paged_typed_segments
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-data-query cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-data-query/Cargo.toml -p marrow-lsp-core data_query_errors_are_typed
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-data-query cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-data-query/Cargo.toml -p marrow-mcp
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-data-query/editors/vscode run check
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-data-query cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-data-query/Cargo.toml --workspace --all-features
```

### Task 5: Gate DAP Launch Against Stable Facts

**Files:**
- Modify: `crates/marrow-dap/src/project.rs`
- Modify: `crates/marrow-dap/src/run.rs`
- Modify: `crates/marrow-dap/src/session.rs`
- Modify: `crates/marrow-dap/tests/debug_session.rs`
- Do not modify: `editors/vscode/package.json` while explicit entry and non-empty args remain blocked.

- [ ] **Step 1: Write or refresh DAP tests for explicit entry and non-empty typed args**

In lane `marrow-lsp-dap-fact-gate`, keep tests named `launch_rejects_explicit_entry_strings_until_canonical_facts_exist` and `launch_blocks_non_empty_args_by_default_until_typed_facts_exist`. If Marrow later exposes stable protocol DTOs, replace these with red tests for the new supported behavior. Tests must assert structured launch responses and status fields, not prose.

- [ ] **Step 2: Run the focused tests and confirm the current blockers fail**

Run:

```sh
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-dap-fact-gate cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-dap-fact-gate/Cargo.toml -p marrow-dap launch_rejects_explicit_entry_strings_until_canonical_facts_exist
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-dap-fact-gate cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-dap-fact-gate/Cargo.toml -p marrow-dap launch_blocks_non_empty_args_by_default_until_typed_facts_exist
```

- [ ] **Step 3: Adopt only stable session facts**

Use Marrow session plumbing for configured `defaultEntry`, isolated writes, and store/session metadata only where the API is stable enough for release. Keep explicit protocol entry strings and non-empty protocol args blocked until Marrow exposes canonical entry and typed argument DTOs.

- [ ] **Step 4: Keep true debugger gaps blocked**

Line breakpoint arming, frame-local watch evaluation, and durable variables stay blocked unless Marrow exposes canonical stop-point, frame-scope, value-expansion, and durable-watch facts.

- [ ] **Step 5: Verify DAP**

Run:

```sh
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-dap-fact-gate cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-dap-fact-gate/Cargo.toml -p marrow-dap
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-dap-fact-gate/editors/vscode run check
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-dap-fact-gate cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-dap-fact-gate/Cargo.toml --workspace --all-features
```

### Task 6: Split Hover And Source-Intelligence Hotspots

**Files:**
- Modify: `crates/marrow-lsp-core/src/hover.rs`
- Create or modify focused hover submodules under `crates/marrow-lsp-core/src/hover/`
- Modify: `crates/marrow-lsp-core/src/navigation.rs`
- Modify: `crates/marrow-lsp-core/src/completion.rs`
- Modify: `crates/marrow-lsp-core/src/signature_help.rs`
- Test: `crates/marrow-lsp-core/src/hover/tests.rs`
- Test: `crates/marrow-lsp-core/src/navigation/tests.rs`
- Test: `crates/marrow-lsp-core/src/completion/tests.rs`
- Test: `crates/marrow-lsp-core/src/signature_help/tests.rs`

- [ ] **Step 1: Add characterization tests only where behavior is not already covered**

In lane `marrow-lsp-source-intel-shape`, no production refactor starts until a focused existing or new test protects the behavior being moved. The lane starter must name the exact test before editing.

- [ ] **Step 2: Split fact collection from rendering**

The collection layer returns typed, transport-neutral presentation facts. The renderer produces Markdown or LSP structures.

- [ ] **Step 3: Remove duplicate semantic classifiers**

Any classifier that decides language meaning must be replaced by Marrow facts or blocked. Lexical presentation helpers must be named as lexical and tested as presentation only.

- [ ] **Step 4: Verify source intelligence**

Run:

```sh
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-source-intel-shape cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-source-intel-shape/Cargo.toml -p marrow-lsp-core hover::tests
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-source-intel-shape cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-source-intel-shape/Cargo.toml -p marrow-lsp-core navigation::tests
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-source-intel-shape cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-source-intel-shape/Cargo.toml -p marrow-lsp-core completion::tests
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-source-intel-shape cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-source-intel-shape/Cargo.toml -p marrow-lsp-core signature_help::tests
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-source-intel-shape cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-source-intel-shape/Cargo.toml -p marrow-lsp-core semantic_tokens::tests
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-source-intel-shape cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-source-intel-shape/Cargo.toml --workspace --all-features
```

### Task 7: Release CI And Artifact Gate

**Files:**
- Modify or create: `.github/workflows/*.yml`
- Conditional modify: `editors/vscode/package.json` only for package scripts invoked by the workflow.
- Conditional modify: `README.md` only for release gate documentation required by the workflow change.

- [ ] **Step 1: Add a CI workflow that runs the release gate before packaging**

In lane `marrow-lsp-release-ci`, the workflow must run Rust fmt/build/test/clippy, unsafe scan, npm ci/check/compile/audit, and package smoke.

- [ ] **Step 2: Keep release packaging separate from quality checks**

Release artifacts may depend on a passing quality workflow, but the packaging workflow must not be the only quality signal.

- [ ] **Step 3: Verify workflow syntax and local package commands**

Run:

```sh
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-release-ci cargo fmt --manifest-path /Users/scottwilliams/Dev/marrow-lsp-release-ci/Cargo.toml --all -- --check
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-release-ci cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-release-ci/Cargo.toml --workspace --all-features
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-release-ci cargo clippy --manifest-path /Users/scottwilliams/Dev/marrow-lsp-release-ci/Cargo.toml --workspace --all-targets --all-features -- -D warnings
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-release-ci/editors/vscode ci
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-release-ci/editors/vscode run check
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-release-ci/editors/vscode run compile
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-release-ci/editors/vscode audit --audit-level=moderate
```

### Task 8: Final Evidence Lane

**Files:**
- Modify only docs needed for release notes or final status.

- [ ] **Step 1: Rebase onto current `main`**

Check both `marrow-lsp` and `marrow` status and recent commits before final gates.

- [ ] **Step 2: Run the full gate from a clean checkout**

Run:

```sh
git diff --check
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-final-evidence cargo fmt --manifest-path /Users/scottwilliams/Dev/marrow-lsp-final-evidence/Cargo.toml --all -- --check
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-final-evidence cargo build --manifest-path /Users/scottwilliams/Dev/marrow-lsp-final-evidence/Cargo.toml --workspace --all-features
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-final-evidence cargo test --manifest-path /Users/scottwilliams/Dev/marrow-lsp-final-evidence/Cargo.toml --workspace --all-features
CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/marrow-lsp-final-evidence cargo clippy --manifest-path /Users/scottwilliams/Dev/marrow-lsp-final-evidence/Cargo.toml --workspace --all-targets --all-features -- -D warnings
! rg -n '\bunsafe\b' crates editors/vscode/src
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-final-evidence/editors/vscode ci
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-final-evidence/editors/vscode run check
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-final-evidence/editors/vscode run compile
npm --prefix /Users/scottwilliams/Dev/marrow-lsp-final-evidence/editors/vscode audit --audit-level=moderate
```

- [ ] **Step 3: Run absence scans**

Scan for stale prototype terms, stale blockers, unsafe, raw path semantics, TypeScript data derivation, prose-only test assertions, and oversized dispatchers.

- [ ] **Step 4: Get final soundness and idiom/spec reviews**

Both reviewers must inspect the final diff and the evidence packet. Fix every finding and re-review.

- [ ] **Step 5: Produce the final release-readiness packet**

Report exact base/head, changed files, gate outputs, npm audit result, reviewer verdicts, resolved blockers, remaining explicit Marrow blockers, and whether the allowed claim is `fully production-ready` or `production-safe integration with blocked non-production surfaces`.
