# Marrow-LSP Production Readiness Roadmap

This roadmap takes `marrow-lsp` from its verified current state to production-ready. It supersedes
the prior `production-readiness-roadmap.md` in place and keeps its contracts: Marrow owns all
`.mw` semantics; `marrow-lsp-core` consumes canonical Marrow facts and produces transport-neutral
presentation data; the fact-consumption ledger is updated in the same change as any surface
graduation, deletion, or newly discovered blocker; no compatibility shims, local classifiers, or
prototype bridges keep old behavior alive. Landing this document in-repo is Lane S0.4.

## Current Release State

`marrow-lsp` HEAD `90482518` does not compile against marrow HEAD `f89f30d9` until Lane S0.0 (the
committed repair) lands: `TreeStore::open_read_only` is now `pub(crate)` and
`marrow_project::ConfigError` gained a `position` field. The standing release claim
(`production-safe integration with blocked non-production surfaces`) is invalid until S0 completes.

The pinned upstream is marrow `f89f30d9db9f3a5fa6ac087619ea36091bcf17a5`. Against that pin, after
the repair, the full bar is green except one adapted test (Lane S0.3). Every surface in the
ledger is `ready` or `deleted`; the only unledgered semantic surface is `indentation.rs`
(Lane S0.4 enters it as `presentation-only`).

Do not claim `fully production-ready` until the ledger has no production surface left as
`presentation-only` or `blocked-on-marrow`, and every stage exit gate below has fresh evidence.

## Operating Rules For Lane Agents

Each lane is one reviewed worktree lane under this repo's devx rules. Use an isolated worktree
with its own `CARGO_TARGET_DIR` outside the checkout; spell it in every cargo command with an
explicit `--manifest-path`; delete the target dir when the lane integrates. No `git stash`; no
`cargo clean` as a fix. A behavior change starts from a failing check. The exit bar is: build,
full test suite, `clippy -D warnings` at zero, formatter clean, zero `unsafe`, no new
dependencies without an approved decision, plus the TypeScript `npm run check`/`compile` and
`npm audit --audit-level=moderate` where the lane touches `editors/vscode`. Fresh output only.

## Integration Contract

- Lanes run later as reviewed worktree lanes; nothing merges from this analysis directly.
- Ledger and narrative-doc edits land only through Lane S0.4 (the docs/ledger lane), which owns
  the same-change ledger-update rule. A lane that graduates a surface updates its ledger row in
  its own change; a lane that only reasons about a surface does not edit the ledger.
- Upstream (marrow) asks go only through the upstream handoff register held with the marrow
  program of record. Every `blocked-on` field below that names an upstream fact references a
  register entry by its `UPSTREAM-N` name and adds nothing to the ask. This roadmap schedules the
  LSP-side consumer; it does not design the upstream fact.
- File-disjoint lanes within a stage may run in parallel; lanes that share a file are sequenced.
  The hot files are `backend.rs`, `workspace.rs`, `mcp.rs`, `catalog_binding.rs`, and
  `navigation/indices.rs` — dependency notes below call out the collisions.

## Standing Measurement Harness

The interactive-performance and trust-boundary measurements that anchor the S1/S2/S3 budgets were
produced by drivers currently staged outside the repo (`e1_measure.py`, `e1_deadzone.py`,
`e1_rss.py`, `e5_wedge_probe.py`, and the mw_run/mw_test store-untouched oracle). Lane S5.3 lands
the source-scale synthetic-project generator and the latency-budget harness in
`crates/marrow-lsp-core/tests/` (and `crates/marrow-lsp/tests/`), converting those drivers into
tracked test tooling. Budget constants are seeded from the measured baseline (below) times a
safety factor, and each S1 lane re-runs the relevant budget as its exit evidence. The trust-oracle
(byte/mtime/inode identity of the store across mw_run/mw_test) lands as a CI conformance gate in
Lane S0.2.

Measured baseline (warm cache, p50/p95 ms; tier = module count):

| request | p050 | p200 | p500 | bigfile (1 file, 400 fn) |
|---|---|---|---|---|
| hover | 0.6/1.2 | 2.1/4.9 | 5.2/12.0 | 1.3/2.8 |
| definition | 1.0/1.2 | 3.7/4.1 | 9.5/9.9 | 0.6/0.7 |
| references | 1.0/1.0 | 3.7/3.9 | 9.5/9.8 | 0.7/0.8 |
| semanticTokens/full | 2.6/2.7 | 29.8/30.4 | 174/176 | 307/310 |
| diagnostics (raw) | 750 | 881 | 1178 | 778 |
| post-edit dead zone (edit→fresh) | 788/794 | 984/1035 | 1442/1448 | 836/861 |

Warm numbers are a lower bound: the per-request freshness gate re-reads closed files from disk, so
cold-page-cache, networked, or antivirus-hooked filesystems multiply the file-count terms.

---

## Stage S0 — Unbreak + Pin

**Entry gate:** none (this is the base). **Exit gate:** the pinned pair builds clean; the full
Rust bar and `npm run check`/`compile`/`audit` pass in CI against the recorded marrow rev; the
generated consumed-surface snapshot and Cargo.lock resolve with no un-reviewed drift; this roadmap
and the corrected ledger/AGENTS/README are in-repo.

### Lane S0.0 — Store-access repair (item zero)
- **Provenance:** e2a-2, e2b-0 (both `defect-now`).
- **Status:** written and committed at `cd601a9` (patch `patches/0001-review-pin-repair-for-measurement-roadmap-item-zero.patch`); re-apply as the first integration.
- **Files:** `catalog_binding.rs`, `data_explorer.rs`, `mcp/tests.rs`, `dap/project.rs`, `dap/tests/debug_session.rs`, `workspace.rs`, `Cargo.lock`.
- **Change:** migrate the catalog probe and all test fixtures from raw `TreeStore::open*` to the public `SealedStore::open` + `AccessMode`; stop constructing `ConfigError` locally (consume the new `position` field).
- **Enforcement artifact:** upstream `pub(crate)` visibility already makes a raw downstream open unrepresentable — recurrence is compile-impossible. No local gate needed.
- **Blocked-on:** nothing. **Sizing:** landed.

### Lane S0.1 — Release pin + consumed-surface drift gate (PIN+DG)
- **Provenance:** e2b-1, e2b-5, e2a-1, e8-5 (`release-engineering`), e2b-7 (`polish`, ledger symbol cross-check).
- **Files:** root `Cargo.toml`, new `pins/marrow-rev.toml`, new `xtask/gen-surface` (snapshot generator), `Cargo.lock`, `docs/roadmaps/lsp-fact-consumption-ledger.md`.
- **Change:** record the marrow git rev `f89f30d9db9f3a5fa6ac087619ea36091bcf17a5` and tree hash in a tracked pin file; release builds resolve marrow via that rev (git dep or vendored rev) so a `.vsix` is reproducible; bumps are a deliberate edit. Generate a checked-in `consumed-marrow-surface.snapshot` seeded from the consumed-surface inventory, enumerating the full transitive marrow closure — **9** crates, including the currently-undeclared transitive `marrow-codes`, not the 8 in the dependency table.
- **Harness/oracle:** `xtask/gen-surface` walks `cargo metadata` for every path/git `marrow-*` crate and the consumed symbol set.
- **Enforcement artifact:** a CI job regenerates the snapshot against the pinned rev and fails on any un-reviewed delta; a second assertion checks that every marrow symbol the ledger names is actually linked (catches the ledger overstating `source_saved_path_completion_fact_at`, which no prod file references).
- **Before/after:** today path deps + an inert `version = "0.1.0"` lock give zero drift protection against a daily-rewritten upstream; after, an upstream API rewrite is a reviewable snapshot delta, not a silent compile break.
- **Release gate (e8-5):** a package step that refuses to build a `.vsix` while any marrow dependency is a `path =` dependency, so a `0.1.0` "Initial release" cannot ship against a floating, non-reproducible upstream. Until pinning lands, the `0.1.0` CHANGELOG/version is a pre-release marker.
- **Blocked-on:** nothing (rev selected). **Sizing:** one lane. Shares `Cargo.toml`/`Cargo.lock` with S0.0 — sequence after it.

### Lane S0.2 — CI bootstrap (Required Fresh Evidence bar)
- **Provenance:** e6-0, e6-1, e7-4, e8-4 (`release-engineering`), e6-8 (`polish`, npm audit), e5-5 (`release-engineering`, store-untouched oracle).
- **Files:** new `.github/workflows/ci.yml`, root `Cargo.toml` (comment naming the pin source), new `crates/*/tests` wiring for the store-untouched oracle.
- **Change:** a PR+push workflow running the Required Fresh Evidence bar (fmt, build, test, `clippy -D warnings`, `! rg unsafe`, `npm ci`/`check`/`compile`, `npm audit --audit-level=moderate`) with `CARGO_TARGET_DIR` outside the checkout. The job checks out marrow at the S0.1-recorded rev into a sibling `../marrow` so path deps resolve as in local dev, and asserts the checked-out rev matches the pin (fail closed if absent or mismatched). Add a `cargo build --locked` step so a stale committed lock fails visibly. Wire the mw_run/mw_test store-untouched oracle as a conformance gate that fails if any store/lock/sidecar byte, mtime, or inode is perturbed.
- **Enforcement artifact:** the workflow file itself is the drift gate; the roadmap's report-only audit and byte-identity obligations become automated fail-closed gates.
- **Blocked-on:** S0.1 (needs the recorded rev + checkout indirection). **Sizing:** one lane.

### Lane S0.3 — Run-DTO span-contract adaptation
- **Provenance:** e2a-0 (`release-engineering`).
- **Files:** `crates/marrow-lsp-core/src/mcp/tests.rs` (and any run-DTO consumer assuming a span).
- **Change:** treat the run DTO `source_span` as optional everywhere; accept `null` for user-supplied nonexistent entries. The upstream `Option<RunSourceSpanJson>` with documented null-for-locationless semantics is the type-level enforcement.
- **Enforcement artifact:** the adapted test is the only red test at the pin; its green state is a pin-compat assertion.
- **Blocked-on:** nothing (upstream fact already present; downstream adaptation). **Sizing:** small; can run parallel to S0.2.

### Lane S0.4 — Roadmap, ledger, and contract corrections (DOCS)
- **Provenance:** e8-1 (A1), e8-2 (A2), e8-3 (A4), e8-6 (A7), e8-7 (A5), e8-8 (A6), e8-9 (A10), e8-0 (A3), e2b-7, e1-10 (ledger text), e2b-3 (one-core contract decision).
- **Files:** `docs/roadmaps/production-readiness-roadmap.md` (replace with this document), `docs/roadmaps/lsp-fact-consumption-ledger.md`, `AGENTS.md`, `README.md`, delete `docs/superpowers/plans/`.
- **Changes:**
  - Land this roadmap in place of the old one.
  - Workspace-analysis-cache ledger row: scope committed-lock invalidation to `marrow.liveData` on (the always-on client watchers are only `**/marrow.json` and `**/*.mw`); reframe per-request freshness as a filesystem walk + full-file re-read, not a generation compare (A1/A2, = e1-10 text). The interactive-latency budget gate for that row lands with Lane S1.1.
  - Insert a new `On-type indentation` ledger row as `presentation-only`: local block-open/terminator/visibility keyword classification over re-lexed tokens (`indentation.rs`), a bounded typing aid that drifts if the grammar changes; graduation to `ready` requires the Marrow parser block-structure fact (**UPSTREAM-6**).
  - Correct the Completion row to drop `source_saved_path_completion_fact_at` (upstream-internal, not LSP-consumed).
  - AGENTS.md dependency section: the transport binaries link a bounded set of marrow crates directly (`marrow-run`, `marrow-store`, `marrow-check`, `marrow-project`, `marrow-syntax`, `marrow-terminal`); `marrow-lsp-core` is the sole language-intelligence consumer. README crate list adds `marrow-catalog`/`marrow-json`; README Layout adds `marrow-terminal`; README Status names the intentionally-absent surfaces (data integrity/repair, live saved-data hover/counts, saved-data-backed rename, served-process attach).
  - **One-core contract decision (e2b-3):** resolve `marrow-dap` linking deep runtime/debug/store internals directly — either route DAP's runtime/debug facts through a new `marrow-lsp-core` debug module, or amend AGENTS/ledger to record a bounded DAP exception (DAP may link `marrow_run` runtime-hook + `marrow_check` debug-expr directly), enumerated in the ledger and drift snapshot. This is an owner decision; the contract cannot stay silently false.
- **Enforcement artifacts:** an `indentation.rs` drift test that enumerates the depended-on Marrow `Keyword` variants and fails when the block-opening or visibility keyword set changes; a workspace dependency-shape test asserting the direct marrow-linker set equals exactly `{marrow-lsp-core} ∪ {recorded exceptions}`, so a new accidental transport→marrow link fails loudly.
- **Blocked-on:** the one-core decision needs an owner ruling; the rest is unblocked. **Sizing:** one lane (docs + two architecture tests). Land after S0.1 so the snapshot cross-check backs the ledger corrections.

### Lane S0.5 — Core-owned read-only store seam (STORE-VIEW)
- **Provenance:** e2b-6 (`architecture-gap`).
- **Files:** new `crates/marrow-lsp-core/src/store_view.rs`, `catalog_binding.rs`, `data_explorer.rs`, `crates/marrow-dap/src/debugger.rs`.
- **Change:** introduce one core-owned read-only store-view module that owns all `marrow_store` type contact (`TreeStore`/`SealedStore`, `StoreUid`, `CommitMetadata`, `DataPathSegment`, `SavedKey`, `SavedValue`, `CatalogId`); DAP and data views consume that module, not raw store internals. Insulates core/DAP from the greenfield store redesign.
- **Enforcement artifact:** an architecture test restricting `marrow_store` imports to the single `store_view` module.
- **Before/after:** today store internals are consumed at multiple call sites; after, a store-engine change touches one seam.
- **Blocked-on:** nothing for the seam over `SealedStore`; the lock-free probe consumer rides **UPSTREAM-3** (Lane S1.12). **Sizing:** one lane. Shares `catalog_binding.rs` with S0.0 — sequence after it.

---

## Stage S1 — The Instant Bar

Interim lanes are pure-LSP and keep whole-project recompute; they remove the per-request disk cost
and the post-edit dead zone. The O(project) analysis ceiling is lifted only by the post-upstream
lanes (S1.13/S1.14) once **UPSTREAM-1**/**UPSTREAM-2** land. Lane S5.3 (latency-budget harness)
lands first and gates every exit here.

**Entry gate:** S0 complete; S5.3 harness present with baseline budgets. **Exit gate:** each lane's
named latency budget passes at p500 scale under the harness; the post-edit dead-zone test serves
prior-generation facts; the workspace-cache ledger row matches the shipped freshness mechanism.

### Lane S1.1 — Invalidation token (freshness gate)
- **Provenance:** e1-0 (`architecture-gap`); pairs with e1-10 ledger reconcile.
- **Files:** `crates/marrow-lsp-core/src/workspace.rs` (`latest_matches_snapshot`, `current_analysis_paths`, `fresh_latest`/`fresh_index`), `crates/marrow-lsp/src/backend.rs` (read handlers + `fresh_index_if_available`).
- **Change:** replace the per-request directory-walk-plus-closed-file-reread freshness gate with a monotonic freshness token bumped by document generation (already tracked) plus watched-file/lock events (already wired to `invalidate_analysis`).
- **Enforcement artifact:** a per-request latency-budget test at p500 (target warm hover/nav well under baseline; the disk walk removed from the hot path) **and** privatizing/removing `latest_matches_snapshot` so no read handler can re-introduce the walk.
- **Before/after:** warm hover p500 5.2/12.0 ms, definition/references 9.5 ms, semanticTokens 174 ms (all O(project) disk) → O(1) freshness check.
- **Blocked-on:** nothing. **Sizing:** one lane. Owns the `workspace.rs` freshness path; sequence S1.2/S1.3 around it (all three touch `workspace.rs`/`backend.rs`).

### Lane S1.2 — Navigation from snapshot (kills the second disk pass and 0:0 mislocate)
- **Provenance:** e1-1 (`architecture-gap`), e4-9 (`defect-now`, unreadable-target 0:0), e1-8 (`polish`, workspace/symbol LineIndex-per-file).
- **Files:** `crates/marrow-lsp-core/src/navigation/indices.rs`, `crates/marrow-lsp/src/backend.rs` (`snapshot_indices`).
- **Change:** build `FileIndex` from the snapshot's in-memory `file.source` (self-consistent with the spans, zero disk I/O), lazily for only the files in the result; a read failure becomes a typed absence (skip the file / no location) instead of an empty-index `0:0` fallback. Apply the same lazy build to `workspace/symbol`, which today builds a `LineIndex` for every project file up front including files with zero matching symbols (e1-8).
- **Enforcement artifact:** a navigation latency budget at p500, plus a test that a deleted/unreadable target yields no location, never `0:0`.
- **Before/after:** today `SnapshotIndices::new` does a full second `read_to_string` pass over every closed file while the analysis mutex is held; after, zero disk I/O and no bogus `0:0`.
- **Blocked-on:** nothing. **Sizing:** one lane. Shares `backend.rs` with S1.1 — sequence.

### Lane S1.3 — Analysis off the pump + last-good serving
- **Provenance:** e1-2 (`architecture-gap`).
- **Files:** `crates/marrow-lsp/src/backend.rs` (`recompute_and_publish`, `RecomputeContext`), `crates/marrow-lsp-core/src/workspace.rs` (retain last-good snapshot).
- **Change:** move `analyze_project` to `spawn_blocking` or a dedicated single analysis actor so the tokio pump never holds a lock across CPU-bound work (gopls generation / rust-analyzer salsa-cancellation prior art); read handlers serve the last-good snapshot during a recompute instead of `Ok(None)`.
- **Harness/oracle:** extend the existing `TestRecomputeGate` to park a slow analysis.
- **Enforcement artifact:** a fault-injected slow-analysis test asserting hover/definition return prior-generation facts while a recompute is parked, not `Ok(None)`.
- **Before/after:** 0.8–1.4 s post-edit dead zone (blank hover/nav/workspaceSymbol, empty-program completion fallback) on 10/10 samples per tier → prior-generation facts served throughout.
- **Blocked-on:** interim is pure-LSP; eliminating the recompute cost itself is Lane S1.13. **Sizing:** one lane. Shares `backend.rs`/`workspace.rs` — sequence with S1.1/S1.2.

### Lane S1.4 — Error-resilient hover/navigation reads
- **Provenance:** e1-6 (`architecture-gap`).
- **Files:** `crates/marrow-lsp-core/src/workspace.rs` (retain last-good facts for hover/nav, not only completion), `crates/marrow-lsp/src/backend.rs` (read handlers fall back to last-good when the latest recompute lost the module).
- **Change:** retain the last-good facts snapshot for hover/nav and fall back when the newest recompute lost the module, matching completion's existing error-resilient fallback.
- **Enforcement artifact:** a fixture test that introduces a one-line error and asserts hover/definition on an unaffected symbol still resolve.
- **Before/after:** today one invalid line (e.g. a stray `;;` doc comment) blanks all hover/nav for the file indefinitely; after, unaffected symbols resolve through the error.
- **Blocked-on:** interim last-good retention is pure-LSP; per-symbol resilience through a genuinely broken module escalates to **UPSTREAM-7** only if the interim proves insufficient. **Sizing:** one lane.

### Lane S1.5 — Shared (Arc) document snapshots
- **Provenance:** e1-5 (`architecture-gap`).
- **Files:** `crates/marrow-lsp-core/src/documents.rs`, `positions.rs`.
- **Change:** store buffer text as `Arc<str>` and share `LineIndex`/lexed/parsed via `Arc` in `DocumentSnapshot` so a snapshot is a refcount bump, not an O(file-size × open-buffers) deep clone; stop storing text twice per document.
- **Enforcement artifact:** an allocation/latency budget test on a large open buffer.
- **Before/after:** every per-document request and every recompute deep-clones text + LineIndex + LexedSource + ParsedSource; after, cheap sharing.
- **Blocked-on:** nothing. **Sizing:** one lane; file-disjoint from S1.1–S1.4.

### Lane S1.6 — Semantic tokens delta + no double-compute
- **Provenance:** e1-4 (`architecture-gap`).
- **Files:** `crates/marrow-lsp/src/backend.rs` (capability + `semantic_tokens_full`), `crates/marrow-lsp-core/src/semantic_tokens*.rs`, ledger row for Semantic tokens.
- **Change:** advertise and implement `semanticTokens/full/delta` with a `result_id` cache (and optionally range); skip the parse-only computation when fresh analyzed facts already exist. Reconcile the ledger row to the real capability set (the ledger claims delta encoding that does not exist).
- **Enforcement artifact:** a large-open-file token latency budget test; the ledger row matches the advertised capabilities.
- **Before/after:** full-document only, double-computed on the fresh path, `result_id: None`; bigfile 307 ms → delta-encoded incremental.
- **Blocked-on:** nothing (encoding is LSP-owned). **Sizing:** one lane. Shares `backend.rs` — sequence after S1.1/S1.3.

### Lane S1.7 — Request cancellation and drop-on-supersede
- **Provenance:** e1-7 (`polish`).
- **Files:** `crates/marrow-lsp/src/backend.rs` (transport pump).
- **Change:** honor `$/cancelRequest`; drop a request whose document generation is already superseded before doing disk/compute work.
- **Enforcement artifact:** a test firing `$/cancelRequest` against an in-flight hover/completion asserting it returns cancelled without running the analysis body (uses the S5.2 interleaving harness).
- **Blocked-on:** nothing; strongest once S1.1/S1.2 shrink per-request cost. **Sizing:** one lane. Shares `backend.rs` — sequence late in S1.

### Lane S1.8 — Catalog probe retains last-accepted identity
- **Provenance:** e3-0 (`polish`).
- **Files:** `crates/marrow-lsp-core/src/catalog_binding.rs` (`StoreProbe::Unavailable` branch, `CatalogBindingCache`).
- **Change:** on a transient `Unavailable` (writer flock held), serve the cached `(key, accepted)` identity rather than resetting to first-run `None` — a transient lock is not evidence the store reset.
- **Enforcement artifact:** an integration test that holds a writer flock across a recompute and asserts the cached accepted identity + key are retained (never reset to `None`) while `Locked`.
- **Before/after:** any concurrent CLI writer flips catalog identity to first-run and empties the Saved Resource Inspector; after, identity/data-view stays last-accepted.
- **Blocked-on:** nothing for retain-last; a fully writer-safe refresh additionally wants **UPSTREAM-3**. **Sizing:** one lane. Shares `catalog_binding.rs` with S0.5 — sequence after it.

### Lane S1.9 — Server-side dynamic watch registration
- **Provenance:** e3-2 (`architecture-gap`), e8-1 (A1, `**/marrow.lock` watcher implementation).
- **Files:** `crates/marrow-lsp/src/backend.rs` (`initialized` → `register_capability` for `workspace/didChangeWatchedFiles` with globs `**/marrow.json`, `**/*.mw`, `**/marrow.lock`, and the store file), `editors/vscode/src/extension.ts` (drop the ad-hoc client registry).
- **Change:** register watchers server-side so freshness is client-agnostic; add the `**/marrow.lock` watcher that is missing from the default (non-liveData) client config, feeding the committed-lock invalidation branch.
- **Enforcement artifact:** a bare-LSP integration test (no VS Code) that touches an external `.mw` and `marrow.lock` and asserts the server invalidates; a client test asserting a `marrow.lock` change produces a `didChangeWatchedFiles`.
- **Before/after:** watch is entirely TS-side, so non-VS-Code clients get zero invalidation and default VS Code never watches `marrow.lock`; after, invalidation is server-driven and covers the lock in the default config.
- **Blocked-on:** nothing. **Sizing:** one lane. Shares `backend.rs` — sequence within S1; feeds S1.1's token bump sources.

### Lane S1.10 — Store-file change as an invalidation input
- **Provenance:** e3-1 (`architecture-gap`).
- **Files:** `crates/marrow-lsp/src/backend.rs` (`watched_project_changes` + schedule path), `editors/vscode/src/extension.ts` (store-file watcher unconditional, not only under liveData).
- **Change:** make a store-file change a debounced analysis-invalidation event that bumps the S1.1 token and re-binds catalog identity (not necessarily a full source recheck).
- **Enforcement artifact:** a harness test that opens the editor, runs a CLI `marrow run` commit, and asserts the editor re-binds to the new `commit_id` within a bounded time with no intervening edit.
- **Before/after:** store commits never retrigger analysis; cached identity is stale until an unrelated keystroke → bounded re-bind within the debounce window.
- **Blocked-on:** **UPSTREAM-3** to read the new identity while a writer may still hold the store (interim best-effort re-bind via `SealedStore`). **Sizing:** one lane. Shares `backend.rs`/`extension.ts` with S1.9 — sequence after it.

### Lane S1.11 — Data-view refresh debounce + session cache
- **Provenance:** e3-5 (`architecture-gap`).
- **Files:** `editors/vscode/src/extension.ts` (coalesce `refreshWatchTargets`/`dataProvider.refresh` behind a debounce; gate on view visibility and on watch-target-set change), `crates/marrow-lsp-core/src/store.rs` + `backend.rs` (cache the SavedDataSession/checked-program keyed by source+store generation).
- **Change:** debounce data-view refresh so rapid CLI commits do not storm read-only session opens; cache the session so a refresh does not re-check from disk.
- **Enforcement artifact:** a harness test driving rapid CLI commits with the inspector active asserting bounded store opens per second and unaffected hover latency.
- **Before/after:** one session open per publish/commit → ≤1 per debounce window.
- **Blocked-on:** the debounce is pure-client; the session-cache keying is cleaner with **UPSTREAM-3**'s generation fact but can key on the existing `DataSnapshotStamp` meanwhile. **Sizing:** one lane.

### Lane S1.12 — Probe only on store change
- **Provenance:** e3-4 (`architecture-gap`); upstream ask e3-3 (`blocked-on-marrow`, = **UPSTREAM-3**).
- **Files:** `crates/marrow-lsp-core/src/catalog_binding.rs`.
- **Change:** gate the redb open on a store-epoch change instead of opening every debounced recompute.
- **Enforcement artifact:** a test asserting a pure source edit with an unchanged store performs zero redb opens (grep-gate on `SealedStore::open` in the probe path).
- **Before/after:** 1 redb open (full integrity ladder) per recompute, adding writer contention on the edit hot path → 0 opens when the store epoch is unchanged.
- **Blocked-on:** **UPSTREAM-3** (lock-free store-generation/epoch fact). **Sizing:** one lane. Shares `catalog_binding.rs` with S1.8 — sequence after it; do not start until the register entry lands.

### Lane S1.13 — Incremental / source-only reanalysis
- **Provenance:** e1-3 (`blocked-on-marrow`).
- **Files:** `crates/marrow-lsp-core/src/workspace.rs` (`recompute_from_snapshot`), `crates/marrow-lsp/src/backend.rs` (debounce).
- **Interim (pure-LSP, ships in S1):** make the 700 ms debounce adaptive (shorter for small projects); skip test-module checking on the interactive keystroke path so recompute is source-only.
- **Post-upstream:** dependency-scoped / single-file reanalysis keyed on generation tokens.
- **Enforcement artifact:** a per-tier diagnostics-latency budget test that fails if p500 diagnostics exceed the ceiling.
- **Before/after:** fixed 700 ms debounce + whole-project (incl. tests) re-check floors diagnostics at ~0.75–1.2 s and grows O(project) → interim reduces the constant; the O(project) ceiling lifts only post-upstream.
- **Blocked-on:** **UPSTREAM-1** for the incremental path. **Sizing:** interim one lane now; post-upstream lane scheduled when UPSTREAM-1 lands.

### Lane S1.14 — Entity-query funnel migration (L11)
- **Provenance:** e2b-4 (`architecture-gap`), e1-9 (`polish`, per-call binding-index rebuild in the uncached public `hover()` with no production caller — delete or fold into the entity API).
- **Files:** `hover/*`, `navigation/*`, `completion.rs`, `semantic_tokens.rs`, `signature_help.rs`, `symbols.rs`, `mcp.rs` (~25 production call sites across ~9 core modules).
- **Change:** migrate the per-surface `*_fact_at` funnels to Marrow query-native facts (`resolve_at`-style entities keyed to a snapshot/version token) when they land, replacing per-call re-derivation with entity queries + thin renderers.
- **Enforcement artifact:** the ledger L11 rows flip from category names to named APIs, gated by the S0.1 drift snapshot. Removes the per-call binding-index rebuild (also the concern behind the uncached public `hover()`).
- **Before/after:** the funnels re-derive from the whole-project checked program per request; after, ~a dozen entity queries with thin renderers.
- **Blocked-on:** **UPSTREAM-2** (query-native analysis fact API + snapshot/version token). The snapshot-token requirement is the single addition this review folds into the L11 charter. **Sizing:** one paired lockstep lane (marrow + marrow-lsp), scheduled when UPSTREAM-2 lands; the cursor-resolution + generation-binding shape lands on all surfaces at once.

---

## Stage S2 — Robustness + Protocol Conformance

**Entry gate:** S0 complete (S1 not required, but S2 hot-path panic isolation should precede any
release). **Exit gate:** a hot-path handler panic does not kill the server; a looping run does not
wedge the MCP server and a second call is answered; every DAP teardown path runs
`terminate_session`; MCP `isError`/versioning and DAP async-pause tests pass.

### Lane S2.1 — LSP execution model + panic isolation
- **Provenance:** e4-0 (`architecture-gap`).
- **Files:** `crates/marrow-lsp/src/backend.rs`, `crates/marrow-lsp/src/main.rs`.
- **Change:** per-handler `std::panic::catch_unwind` boundary returning a jsonrpc error instead of unwinding; move analysis-heavy handlers off the single tower-lsp pump (`spawn_blocking`/dedicated analysis thread) so heavy sync work cannot freeze all I/O.
- **Harness/oracle:** the S5.1 deterministic (virtual-time) non-blocking harness.
- **Enforcement artifact:** a panic-injection test asserting the server survives one bad request and still answers the next; a latency-budget test at the spawn_blocking boundary.
- **Before/after:** any of the 8 hot-path handler `.expect`s panicking kills the whole editing session; after, one bad request is isolated.
- **Blocked-on:** nothing. **Sizing:** one lane. Overlaps S1.3 (`backend.rs` pump) — sequence with it.

### Lane S2.2 — MCP run budget, cancellation, and output cap
- **Provenance:** e5-0 (`defect-now`), e4-1 (`architecture-gap`), e5-2 (`architecture-gap`).
- **Files:** `crates/marrow-mcp/src/main.rs` (transport loop), `crates/marrow-lsp-core/src/mcp.rs` (`run_entry`/`run_tests`/`locked_host`, output serialization).
- **Interim (landable now):** a wall-clock deadline enforced by a watchdog thread/subprocess kill that returns a typed `budget_exceeded` DTO; an output-byte cap on captured stdout with an `output_truncated: true` flag.
- **Post-upstream:** a graceful in-process cooperative step/instruction cap; bounded-render truncation of large structured return values.
- **Harness/oracle:** the wedge probe (`e5_wedge_probe.py`) graduated into a test that runs a `while true` fixture.
- **Enforcement artifact:** a latency-budget test asserting a looping `mw_run` returns a typed budget-exceeded DTO within N seconds **and** a second tool call is answered; a print-loop/large-return run comes back under the byte cap with the flag set.
- **Before/after:** a single looping `mw_run` wedges the single-threaded MCP server permanently (empirically confirmed) → bounded wall-clock (default e.g. 30 s, operator-set) + output cap.
- **Blocked-on:** the graceful in-process cap is **UPSTREAM-4** (run-session cooperative interrupt); the value-DTO truncation is **UPSTREAM-5**. **Sizing:** one lane; the interim ships without the register entries.

### Lane S2.3 — MCP persistent workspace cache
- **Provenance:** e4-7 (`architecture-gap`).
- **Files:** `crates/marrow-lsp-core/src/mcp.rs`, `crates/marrow-mcp/src/server.rs`.
- **Change:** a per-process `Workspace` cache keyed by project root + source/catalog generation (mirroring the LSP binding-index cache) so tool calls are not cold `Workspace::new()` + recompute + analyze every time.
- **Enforcement artifact:** an invalidation test proving a source change busts the cache + a latency test on a multi-module fixture.
- **Before/after:** cold full-project analyze on every tool call → warm cache with generation-keyed invalidation.
- **Blocked-on:** nothing (the LSP owns the pattern to mirror). **Sizing:** one lane. Shares `mcp.rs` with S2.2 — sequence.

### Lane S2.4 — DAP async pause + live breakpoint edits
- **Provenance:** e4-2 (`defect-now`), e4-3 (`architecture-gap`).
- **Files:** `crates/marrow-dap/src/session.rs`, `crates/marrow-dap/src/debugger.rs`.
- **Change:** a shared stop-at-next-statement `AtomicBool` read by the run-thread hook between statements (mirroring the existing terminate flag) so `pause` on a running thread produces a `stopped` event; store the requested breakpoint set unconditionally and push it to the run thread via a breakpoint-swap channel readable outside park, so a breakpoint toggled mid-run takes effect on the next stop.
- **Enforcement artifact:** a test that `pause` during a `continue` produces a `stopped` event; a test that a breakpoint added mid-continue takes effect on the next stop.
- **Before/after:** `pause` is non-functional for a running thread and mid-run breakpoints are silently dropped → both work through the statement-boundary hook.
- **Blocked-on:** nothing (the hook that services terminate can service pause). **Sizing:** one lane.

### Lane S2.5 — DAP guaranteed teardown
- **Provenance:** e4-4 (`defect-now`).
- **Files:** `crates/marrow-dap/src/main.rs`, `crates/marrow-dap/src/session.rs`.
- **Change:** an RAII guard (or explicit terminate on every `serve()` exit path including `Disconnected`) that always runs `terminate_session`, so an EOF/broken-pipe disconnect does not orphan the run thread or skip `ProjectSession` Drop (isolated-write/lock cleanup).
- **Enforcement artifact:** a test that dropping the input stream mid-run triggers `ProjectSession` cleanup.
- **Blocked-on:** nothing. **Sizing:** one lane. Shares `session.rs` with S2.4 — sequence.

### Lane S2.6 — DAP bounded teardown
- **Provenance:** e4-5 (`defect-now`).
- **Files:** `crates/marrow-dap/src/session.rs`.
- **Change:** a bounded join-with-timeout that detaches the run thread after a deadline, so `terminate`/`disconnect` cannot block the adapter indefinitely on a long-running statement.
- **Enforcement artifact:** a test with a long-running statement asserting `terminate` returns within the budget.
- **Blocked-on:** graceful force-unwind of a truly unbounded statement/blocking builtin needs **UPSTREAM-4** (same fact as S2.2); the detach-after-timeout interim ships now. **Sizing:** one lane. Shares `session.rs` — sequence with S2.4/S2.5.

### Lane S2.7 — DAP panic-visible outcomes
- **Provenance:** e4-8 (`architecture-gap`).
- **Files:** `crates/marrow-dap/src/session.rs`.
- **Change:** distinguish a join `Err` (panic) from `Ok(None)` and emit `output(stderr)` + `exited(nonzero)` on panic, so a run-thread panic is not indistinguishable from clean program end.
- **Enforcement artifact:** a test injecting a run-thread panic asserting a visible error surface (stderr + nonzero exit).
- **Blocked-on:** nothing. **Sizing:** one lane. Shares `session.rs` — sequence with S2.4–S2.6.

### Lane S2.8 — Data-view typed-error surfacing
- **Provenance:** e4-6 (`defect-now`).
- **Files:** `crates/marrow-lsp/src/backend.rs`, `editors/vscode/src/savedResourceInspector.ts`.
- **Change:** check `result.error` before the boundary branch in TypeScript (a boundary cannot be trusted on an error), so typed Path/Store errors render as distinct blocked nodes instead of every real error collapsing to "data changed; refresh" (which leaves the blocked node dead code).
- **Enforcement artifact:** a fixture test rendering a Path error and a Store error as distinct blocked nodes rather than the changed-node.
- **Blocked-on:** nothing. **Sizing:** one lane.

### Lane S2.9 — MCP error-envelope truthfulness
- **Provenance:** e4-10 (`architecture-gap`), e5-6 (`polish`).
- **Files:** `crates/marrow-mcp/src/main.rs` (`tool_result`), `crates/marrow-lsp-core/src/mcp.rs`.
- **Change:** thread a tool-internal-failure bit from the core result into `isError`, so transport/IO failures read as errors to agents; a Marrow diagnostic result (a program that legitimately fails to check) stays `isError:false`.
- **Enforcement artifact:** a test that an unreadable project yields `isError:true` while a check-failing program stays `isError:false`.
- **Before/after:** `isError` is always false, hiding tool-internal failures from agents keying on it.
- **Blocked-on:** nothing. **Sizing:** one lane. Shares `mcp.rs`/`main.rs` with S2.2 — sequence.

### Lane S2.10 — MCP result-shape versioning
- **Provenance:** e4-13 (`polish`).
- **Files:** `crates/marrow-lsp-core/src/mcp.rs`, `crates/marrow-mcp/src/server.rs`.
- **Change:** a `profile_version` on every tool result (the `check`/`type_at`/`run` top-level shapes currently carry none) so agents pinning result shapes can detect breaking renames.
- **Enforcement artifact:** a golden-snapshot drift test over all 12 tool shapes.
- **Blocked-on:** nothing. **Sizing:** one lane. Shares `mcp.rs` — sequence with S2.9.

### Lane S2.11 — MCP protocol-version negotiation
- **Provenance:** e4-14 (`polish`).
- **Files:** `crates/marrow-mcp/src/server.rs`, `crates/marrow-mcp/src/main.rs`.
- **Change:** echo/negotiate the client-requested `protocolVersion` (or reject unsupported) instead of hardcoding `2025-06-18`.
- **Enforcement artifact:** a test with a mismatched requested version.
- **Blocked-on:** nothing. **Sizing:** one lane.

### Lane S2.12 — LSP exit-code conformance
- **Provenance:** e4-11 (`polish`).
- **Files:** `crates/marrow-lsp/src/main.rs`, `crates/marrow-lsp/src/backend.rs`.
- **Change:** track shutdown-seen state (a shared flag `shutdown()` sets and the ExitWatcher reads) and exit 1 on an unclean exit; flush stdout before the hard exit so pipelined stdout is not dropped.
- **Enforcement artifact:** an `exit.rs` conformance case asserting code 1 for exit-without-shutdown.
- **Blocked-on:** nothing. **Sizing:** one lane. Pairs with S2.13.

### Lane S2.13 — ExitWatcher hot-path guard
- **Provenance:** e4-12 (`polish`).
- **Files:** `crates/marrow-lsp/src/main.rs`.
- **Change:** a cheap length/substring pre-guard (exit frames are tiny and contain `exit`) before fully re-parsing every inbound frame body to JSON.
- **Enforcement artifact:** a test asserting a large `didChange` body is not fully re-parsed.
- **Blocked-on:** nothing. **Sizing:** small; folds with S2.12 (same file).

---

## Stage S3 — Trust Boundaries

Applies to any deployment where MCP data access is enabled or an untrusted agent drives the
server. The run-availability wedge (S2.2) is a prerequisite trust concern.

**Entry gate:** S2.2 landed (run budget). **Exit gate:** a read-only policy cannot execute a
write; a data tool cannot reach a store outside the allowed root; executable-path settings are
machine-scoped; the store-untouched oracle gate (S0.2) is green.

### Lane S3.1 — Data-access read/write split + audit trail
- **Provenance:** e5-1 (`architecture-gap`).
- **Files:** `crates/marrow-mcp/src/server.rs` (Policy), `crates/marrow-mcp/src/main.rs` (flag parsing + doc), `crates/marrow-lsp-core/src/mcp.rs` (`surface_write` gate).
- **Change:** replace the single bool with a typed `Policy { allow_read, allow_write }` so `mw_surface_write` reading a read-only policy is a compile-time-distinct path (the shared bit becomes unrepresentable); the tool envelope `_meta marrow/contract` declares `writeOptIn` distinct from `dataAccess`; add an append-only write-audit record (operation tag + entry file; request kind + `store_uid`/`commit_id` before/after).
- **Enforcement artifact:** a structural test that `mw_surface_write` refuses under a read-only policy; the audit record on every write.
- **Before/after:** one read-framed flag silently authorizes destructive writes to the real store → read and write are separately opted-in and audited.
- **Blocked-on:** nothing (the read-only vs writable session distinction already exists upstream). **Sizing:** one lane.

### Lane S3.2 — MCP root confinement + boundary statement
- **Provenance:** e5-4 (`architecture-gap`).
- **Files:** `crates/marrow-mcp/src/server.rs` + `main.rs` (accept an operator-set allowed root, default to cwd/launch dir), `crates/marrow-lsp-core/src/mcp.rs` (reject a `file` resolving outside the allowed root before `load_project`).
- **Change:** pin a workspace root so a data-enabled agent cannot reach any path-reachable marrow store; a `file` outside the root is refused before any store open.
- **Enforcement artifact:** a test that a data tool with a `file` outside the allowed root returns a typed `path.out_of_root` refusal before any store open; a boundary statement in the tool envelope `_meta` and README declaring exactly what a data-enabled server can reach.
- **Before/after:** the server pins no root — a data-enabled agent reaches any store on the filesystem → confined to the allowed root.
- **Blocked-on:** nothing. **Sizing:** one lane. Shares `mcp.rs`/`server.rs` with S3.1 — sequence.

### Lane S3.3 — Machine-scope executable-path settings
- **Provenance:** e5-3 (`polish`), e6-6 (`release-engineering`).
- **Files:** `editors/vscode/package.json` (`scope: "machine"` or `machine-overridable` on `marrow.server.path` and `marrow.dap.path`), `editors/vscode/src/extension.ts`, declare `capabilities.untrustedWorkspaces`.
- **Change:** prevent a workspace file from silently redirecting the spawned binary (code execution on folder open); make the trust boundary explicit; optionally checksum-verify the bundled binary before spawn.
- **Enforcement artifact:** a `package.json` manifest lint (in the existing `scripts/` check ladder, alongside `checkGrammar.mjs`) that fails if either executable-path setting lacks a machine-class scope — a window-scope guard.
- **Before/after:** window-scoped exe paths let a trusted workspace redirect the launched binary → machine-scoped, unrepresentable in a shipped build.
- **Blocked-on:** nothing. **Sizing:** one lane. Shares `package.json` with S4.3 — sequence.

The store-untouched CI conformance gate (e5-5) is the trust enforcement artifact for the mw_run/mw_test sandbox invariant; it is wired in Lane S0.2 and re-runs on every push.

---

## Stage S4 — Release Pipeline

**Entry gate:** S0.2 CI green; S2 robustness landed (a released binary must not wedge or die on a
bad request). **Exit gate:** five per-platform VSIX artifacts build and install; a packaged
extension drives one live request per transport against the real bundled binaries; version stamps
are locked; marketplace governance metadata is present.

### Lane S4.1 — Five-platform release matrix + version-stamp gate
- **Provenance:** e6-2, e6-4 (`release-engineering`).
- **Files:** new `.github/workflows/release.yml`, `editors/vscode/scripts/package.mjs`, new `editors/vscode/scripts/checkVersionStamp.mjs`.
- **Change:** a 5-entry release matrix (runner + rust triple + `vsce --target`) that drives `bundle.mjs` with `CARGO_BUILD_TARGET` per triple and packages one VSIX per target; `package.mjs` gains an explicit `--target`/triple override so it is not host-locked. A preflight gate asserts `package.json.version == Cargo workspace.package.version == top CHANGELOG heading == git tag`.
- **Enforcement artifact:** a release-job assertion that exactly 5 VSIX artifacts are produced and uploaded (fail if fewer), so a dropped platform cannot ship silently; the version-stamp check as a fail-closed first step.
- **Before/after:** the five-platform matrix is a CHANGELOG promise (`package.mjs` builds only the host target) and versions can drift silently → enforced matrix + locked stamps.
- **Blocked-on:** S0.2 (green gate) + the S0.1 pinned rev (binaries must compile); Linux arm64/x64 and win32 need cross toolchains or per-arch runners. **Sizing:** one lane.

### Lane S4.2 — Packaged-extension end-to-end (unstub)
- **Provenance:** e6-3, e7-5 (`release-engineering`/`architecture-gap`).
- **Files:** new `editors/vscode/test/e2e/*`, `.github/workflows/ci.yml` (packaged-e2e job).
- **Change:** a `@vscode/test-electron` (or headless VSIX-install) job that builds the real host VSIX, installs it, opens a fixture project, and drives one live request per transport (LSP hover on a `.mw`, an MCP tool call, a DAP launch/stop-on-entry) against the real bundled `marrow-lsp`/`marrow-mcp`/`marrow-dap`.
- **Enforcement artifact:** the e2e test replaces the `checkBundleSmoke` fake-binary-bytes stub, so a broken activation event, wrong-arch binary, or transport handshake failure fails CI instead of shipping dead.
- **Before/after:** the cross-stack rung writes fake binary bytes and never launches a real binary → a real packaged launch per transport.
- **Blocked-on:** S0.2 + the S0.1 pinned rev; needs a headless-display runner. **Sizing:** one lane.

### Lane S4.3 — Marketplace governance
- **Provenance:** e6-5 (`release-engineering`).
- **Files:** `editors/vscode/package.json`, `editors/vscode/icons/`, `.github/workflows/release.yml`.
- **Change:** migrate to an org/verified publisher; add a top-level gallery icon; declare `capabilities.untrustedWorkspaces` and `virtualWorkspaces` (both currently implicit-unsupported); add `Debuggers`/`Snippets` to categories; wire `vsce publish` with a PAT secret + provenance.
- **Enforcement artifact:** extend `checkBundleSmoke.mjs` to assert publisher ≠ personal placeholder, top-level icon present, and a capabilities block exists — a `package.json` lint that fails the build if governance metadata regresses.
- **Blocked-on:** owner decision on publisher identity (bus-factor / name-ownership). **Sizing:** one lane. Shares `package.json` with S3.3/S4.1 — sequence.

### Lane S4.4 — Activation watchdog
- **Provenance:** e6-7 (`polish`).
- **Files:** `editors/vscode/src/extension.ts`.
- **Change:** wrap `client.start()` in a timeout that surfaces a visible "server failed to start" message and disposes the client; defer the `dataWatchTargets.refresh()` round-trip past activation so cold-start does not serialize two round-trips.
- **Enforcement artifact:** a behavioral test (in the S4.2 e2e harness or a mocked-vscode unit) that a non-responding stub binary makes activation surface an error within the budget instead of hanging.
- **Before/after:** a hung or wrong-arch binary leaves the extension silently stuck → visible error within the budget.
- **Blocked-on:** nothing; pairs with the S4.2 harness. **Sizing:** one lane.

### Lane S4.5 — Launcher-resolution tests
- **Provenance:** e7-6 (`release-engineering`).
- **Files:** new `editors/vscode/scripts/` resolver test, `editors/vscode/src/extension.ts` (export seam if needed).
- **Change:** a thin behavioral test that mocks the vscode config/extensionPath/extensionMode and asserts each `resolveBinaryPath` branch (configured hit/miss, bundled hit, dev fallback gated by Production mode, undefined-on-missing).
- **Enforcement artifact:** the launcher suite itself; it does not turn TypeScript into a second application.
- **Blocked-on:** nothing. **Sizing:** small.

---

## Stage S5 — Testing Architecture (woven gates)

These lanes are the enforcement spine for the earlier stages. Where a gate must block an earlier
exit, it is called out.

### Lane S5.1 — Deterministic non-blocking harness (virtual time)
- **Provenance:** e7-0 (`release-engineering`).
- **Files:** `crates/marrow-lsp/src/backend.rs` (test module), `crates/marrow-lsp/tests/smoke.rs`.
- **Change:** run the backend non-blocking tests under `#[tokio::test(start_paused = true)]` so `tokio::time::timeout` resolves in virtual time (a returning handler completes at ~0 virtual ms; a blocked one deterministically hits the deadline).
- **Enforcement artifact:** a tidy grep-gate test asserting no bare `Duration::from_millis` wall-clock timeout survives in backend test code.
- **Gate for:** S2.1 (panic isolation / off-pump). Blind wall-clock 20–50 ms timeouts are flaky under load and blind to slow regressions. **Sizing:** self-contained test refactor.

### Lane S5.2 — Cancellation + interleaving + multi-session harness
- **Provenance:** e7-1 (`architecture-gap`).
- **Files:** new `crates/marrow-lsp/tests/concurrency.rs`, `crates/marrow-lsp/src/backend.rs`.
- **Change:** an interleaved-request test driving `didChange` + hover + completion + `$/cancelRequest` through the real Backend under `start_paused` virtual time, asserting no deadlock and that a cancelled request does not corrupt a later one; plus a two-session DAP test.
- **Enforcement artifact:** the harness lands asserting current behavior first, becoming the regression net for S1.7 (cancellation) and S2.1.
- **Gate for:** S1.7, S2.1. **Sizing:** one lane.

### Lane S5.3 — Latency-budget + source-scale fixtures
- **Provenance:** e7-2 (`architecture-gap`).
- **Files:** new `crates/marrow-lsp-core/tests/` scale harness, a synthetic-project generator (N modules × M lines), graduated from the staged measurement drivers.
- **Change:** a latency-budget test asserting warm hover/completion/semantic-token/navigation/diagnostics responses under budget constants seeded from the measured baseline × a safety factor.
- **Enforcement artifact:** the budget test is the exit gate for every S1 lane; a response exceeding budget fails.
- **Gate for:** all of S1 (lands first in S1). **Sizing:** one lane.

### Lane S5.4 — Structural no-local-semantics gate
- **Provenance:** e7-3 (`architecture-gap`).
- **Files:** new `crates/marrow-lsp-core/tests/architecture_gate.rs`, removing the per-file substring scans in the six test modules.
- **Change:** one whole-workspace gate that walks every `src/*.rs` under `marrow-lsp-core` (and the transports) and asserts the denied symbols/dependencies appear nowhere — an import/dependency-level or full-source scan replacing the ~28 hand-picked single-file `include_str!` substring scans.
- **Enforcement artifact:** the gate; a relocated classifier or a new mapper module cannot bypass it.
- **Before/after:** the no-local-semantics invariant is defeatable by relocating a classifier → structural, workspace-wide. **Sizing:** one lane.

### Lane S5.5 — Typed-oracle migration (prose decoupling)
- **Provenance:** e7-7 (`polish`).
- **Files:** `crates/marrow-lsp-core/src/hover/tests.rs`, `completion/tests.rs`.
- **Change:** migrate the ~194 `.contains` assertions on Marrow-rendered doc/signature/type prose to typed hover-fact/DTO oracles where a fact exists; move the irreducible rendered-Markdown assertions into reviewed golden files (a golden update requires a semantic-contract review).
- **Enforcement artifact:** golden files for the render-only remainder; a benign upstream reword updates one golden instead of reddening the suite under the churning path-dep.
- **Blocked-on:** nothing (uses existing hover facts). **Sizing:** one lane.

---

## Graduation Appendix

Every Current-Surface-Matrix row is `ready` or `deleted`; after Lane S0.4 enters `indentation.rs`
as `presentation-only`, the roadmap's "no presentation-only or blocked-on-marrow left" test is
honest. The remaining flips depend on named upstream facts routed through the register.

| Deleted / blocked surface | Flips on | Register entry | Consuming surface |
|---|---|---|---|
| On-type indentation (`presentation-only` → `ready`) | Parser block-structure fact | **UPSTREAM-6** | `indentation.rs` |
| Entity-query funnels (L11 category → named API) | Query-native analysis fact API + snapshot/version token | **UPSTREAM-2** | hover/nav/completion/semantic-tokens/signature/symbols/MCP (~25 sites) |
| Diagnostics latency floor (O(project) → incremental) | Incremental / dependency-cone reanalysis + generation advance | **UPSTREAM-1** | `workspace.rs` recompute + debounce |
| Store-freshness without writer contention | Lock-free store-generation / epoch fact | **UPSTREAM-3** | catalog probe (S1.10/S1.12), data-view refresh |
| Graceful run/DAP interrupt | Run-session step/time budget + typed fault | **UPSTREAM-4** (+**UPSTREAM-5** for value-DTO truncation) | MCP run (S2.2), DAP teardown (S2.6) |
| Error-resilient per-symbol tooling (only if interim fails) | Error-resilient tooling facts | **UPSTREAM-7** (conditional) | hover/nav (S1.4) |
| Data integrity / repair | Production validation facts, drift witnesses, typed repair facts | (blocked table, no register entry) | LSP/MCP/VS Code integrity surfaces |
| Live saved-data hover + record counts | A production live-hover/count contract distinct from the read-only data-view API | (blocked table) | hover + inspector |
| Saved-data-backed editor rename | Marrow editor application target + insertion contract | (blocked table) | `prepareRename`/`rename` |
| DAP attach to served programs | Served-process control-boundary facts | (blocked table) | DAP `attach` + VS Code attach config |
| Served-process / live-debuggee data views | Served-process control boundaries + versioned served snapshots | (blocked table) | inspector + DAP |

Permanent deletions (never graduate — superseded by typed DTOs, not blocked on a fact): MCP
untyped `saved get/children`; TypeScript child-path derivation; local active-call parsing for
signature help; source-spelling identities for saved roots, members, indexes, enum members,
aliases, and physical store keys.

---

## Upstream Register Summary

Every `blocked-on` above that names a fact references one of these by name. The full contract
sketch, consuming surfaces, and program-of-record home are in the upstream handoff register (held with the marrow program of record); the
asks are scheduled into the marrow program, not designed here.

| Entry | Ask | Home | LSP lanes it unblocks |
|---|---|---|---|
| UPSTREAM-2 | Query-native analysis fact API + snapshot/version token | L11 (existing) | S1.14 |
| UPSTREAM-1 | Incremental dependency-cone reanalysis + generation advance + source-only | L15 / D8 (existing) | S1.13 (post-upstream) |
| UPSTREAM-4 | Run-session step/time budget + typed budget-exceeded fault | new memo | S2.2, S2.6 (graceful path) |
| UPSTREAM-3 | Lock-free store-generation / epoch fact | new memo (L5 / marrow.lock area) | S1.10, S1.12 |
| UPSTREAM-6 | Parser block-structure fact | L11 rider / small memo | On-type indentation graduation |
| UPSTREAM-5 | Bounded-render budget for large run return values | fold into UPSTREAM-4 memo | S2.2 (value-DTO half) |
| UPSTREAM-7 | Error-resilient tooling facts | conditional L11 rider | S1.4 escalation (only if interim fails) |
