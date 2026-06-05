# Production Readiness Phase 0 Baseline

Timestamp: 2026-06-05 06:55:52 CDT (-0500)

This tracker records the refreshed Phase 0 red baseline for bringing
`marrow-lsp` onto the live Marrow API surface. No source code was changed.

## Repository State

Marrow repository:

- Path: `/Users/scottwilliams/Dev/marrow`
- Status command: `git -C /Users/scottwilliams/Dev/marrow status --short --branch`
- Status output:
  ```text
  ## main...origin/main
  ```
- HEAD command: `git -C /Users/scottwilliams/Dev/marrow rev-parse HEAD`
- HEAD output:
  ```text
  49556121dc4648dec8cd7e11692a4d85cdaf6d7e
  ```
- Staged diff stat command:
  `git -C /Users/scottwilliams/Dev/marrow diff --cached --stat`
- Staged diff stat output: no output.
- Unstaged diff stat command:
  `git -C /Users/scottwilliams/Dev/marrow diff --stat`
- Unstaged diff stat output: no output.

`marrow-lsp` lane:

- Path: `/Users/scottwilliams/Dev/lane-production-baseline`
- Branch: `codex/production-baseline`
- HEAD command:
  `git -C /Users/scottwilliams/Dev/lane-production-baseline rev-parse HEAD`
- HEAD output:
  ```text
  e18f7994264aee52154da46a51568ebd757d5617
  ```
- No commits were made.

Marrow language boundary checked:

- `/Users/scottwilliams/Dev/marrow/docs/language/README.md` identifies
  `docs/language/` as the canonical Marrow `.mw` reference.
- `/Users/scottwilliams/Dev/marrow/docs/language/resources-and-storage.md`
  says inspection and related tools operate through checked tree-cell facts,
  not backend keys as source semantics.
- `/Users/scottwilliams/Dev/marrow/docs/language/grammar.md` includes
  `inout` parameter mode and `evolve` declarations in v0.1 grammar; removed
  `out`, `lock`, and `merge` syntax should not survive as active LSP semantics.

## Clean-Break Guardrails

- The live Marrow checkout is the source of truth for language and API facts.
- Only the clean-break policy and phase plan in the dirty main
  production-readiness roadmap are authoritative for this lane. Its Current
  Evidence section is superseded by this Phase 0 tracker.
- `/Users/scottwilliams/Dev/lane-production-baseline/docs/roadmaps/lsp-devx-roadmap.md`,
  `/Users/scottwilliams/Dev/lane-production-baseline/docs/roadmaps/lsp-fact-consumption-ledger.md`,
  and `/Users/scottwilliams/Dev/lane-production-baseline/docs/roadmaps/v01-parallel-orchestration.md`
  are historical Phase 1 inputs until reconciled against this baseline and the
  live Marrow checkout.
- Phase 1 agents must delete, replace with canonical Marrow facts, or explicitly
  block prototype paths. They must not preserve production behavior for
  `allowPrototypeArgs`, `allowRawDataInspection`, raw saved-data paths,
  debug/admin prototype contracts, or compatibility behavior.
- Tests that assert prototype contracts are evidence for deletion, replacement,
  or an explicit blocked-on-Marrow result. They must not be weakened just to
  make the current prototype behavior pass.

## Command Outcomes

- PASS: `git -C /Users/scottwilliams/Dev/marrow status --short --branch`
  exited 0 and produced:
  ```text
  ## main...origin/main
  ```
- PASS: `git -C /Users/scottwilliams/Dev/marrow rev-parse HEAD`
  exited 0 and produced:
  ```text
  49556121dc4648dec8cd7e11692a4d85cdaf6d7e
  ```
- PASS: `git -C /Users/scottwilliams/Dev/marrow diff --cached --stat`
  exited 0 with no output.
- PASS: `git -C /Users/scottwilliams/Dev/marrow diff --stat`
  exited 0 with no output.
- PASS: `CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/lsp-phase-0-marrow-final cargo check --manifest-path /Users/scottwilliams/Dev/marrow/Cargo.toml --workspace --all-features`
  exited 0. The output included:
  ```text
  Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.78s
  ```
- FAIL, expected red baseline: `CARGO_TARGET_DIR=/Users/scottwilliams/Dev/.cargo-targets/lsp-phase-0-baseline-final cargo check --manifest-path /Users/scottwilliams/Dev/lane-production-baseline/Cargo.toml --workspace --all-features`
  exited 101. The output included:
  ```text
  Locking 7 packages to latest compatible versions
  error: could not compile `marrow-lsp-core` (lib) due to 62 previous errors
  warning: build failed, waiting for other jobs to finish...
  ```
- PASS after lockfile restore: `git -C /Users/scottwilliams/Dev/lane-production-baseline status --short Cargo.lock`
  exited 0 with no output.
- PASS: `git -C /Users/scottwilliams/Dev/lane-production-baseline diff --check`
  exited 0 with no output.
- PASS: `git -C /Users/scottwilliams/Dev/lane-production-baseline status --ignored=matching --short editors/vscode/node_modules`
  exited 0 with no output.
- PASS: `git -C /Users/scottwilliams/Dev/lane-production-baseline status --short --branch`
  exited 0 and produced:
  ```text
  ## codex/production-baseline
  ?? docs/roadmaps/production-readiness-baseline.md
  ```

No npm command was run. `editors/vscode/node_modules` was absent in the final
ignored-status check.

Marrow check: passed against live HEAD
`49556121dc4648dec8cd7e11692a4d85cdaf6d7e`.

`marrow-lsp` Rust check: failed as the Phase 0 red baseline, exit status 101.

## Rust Failure Families

### Public Store And Tooling Boundary

Representative errors:

- `E0432`: unresolved import `marrow_store::backend::Presence`.
- `E0432`: unresolved import `marrow_store::path`.
- `E0603`: module `backend` is private.
- `E0603`: module `mem` is private.
- `E0603`: module `redb` is private.
- `E0277`: `[u8]` size error on raw backend value rendering after API drift.

Representative files:

- `crates/marrow-lsp-core/src/data_explorer.rs`
- `crates/marrow-lsp-core/src/data_integrity.rs`
- `crates/marrow-lsp-core/src/hover.rs`
- `crates/marrow-lsp-core/src/mcp.rs`
- `crates/marrow-lsp-core/src/store.rs`

Notes: this family should move to `TreeStore`, `SavedKey`,
`DataPathSegment`, and `marrow_check::tooling` APIs. The `E0614` errors in
`data_explorer.rs` where `usize` is dereferenced are likely sibling shape drift
inside the same data-explorer cleanup.

### Removed Saved-Path Runtime Classifiers

Representative errors:

- `E0432`: unresolved imports `SavedPathClass`, `classify_saved_path`,
  `decode_identity_arity`, and `identity_leaf_key_mismatch`.
- `E0433`: cannot find `SavedPathClass` in `marrow_run`.
- `E0425`: cannot find function `classify_saved_path` in crate `marrow_run`.

Representative files:

- `crates/marrow-lsp-core/src/data_explorer.rs`
- `crates/marrow-lsp-core/src/data_integrity.rs`
- `crates/marrow-lsp-core/src/store.rs`

Notes: this should be deleted or replaced with canonical typed Marrow tooling
facts. The LSP must not recreate saved-path classifiers locally.

### Private Alias Helpers

Representative errors:

- `E0603`: function `build_alias_map` is private.
- `E0603`: function `expand_alias` is private.
- `E0603`: function `expand_module_alias` is private.

Representative files:

- `crates/marrow-lsp-core/src/completion.rs`
- `crates/marrow-lsp-core/src/hover.rs`
- `crates/marrow-lsp-core/src/navigation.rs`
- `crates/marrow-lsp-core/src/signature_help.rs`

Notes: source-presentation features should consume public `resolve`,
`scope_at`, `type_at`, and binding-index facts. Alias-only needs should become
blocked-on-Marrow handoffs, not local copies of private checker helpers.

### Resolver Return Shape

Representative errors:

- `E0308`: `resolve_store_by_root` now returns `Option<StoreResource<'_>>`,
  not `Option<(_, _)>`.
- `E0308`: tuple destructuring such as `let Some((store, resource)) = ...`
  no longer matches current Marrow.

Representative files:

- `crates/marrow-lsp-core/src/completion.rs`
- `crates/marrow-lsp-core/src/data_explorer.rs`
- `crates/marrow-lsp-core/src/hover.rs`
- `crates/marrow-lsp-core/src/store.rs`

Notes: callers should use `StoreResource { module, store, resource }`.

### Syntax Surface Alignment

Representative errors:

- `E0599`: no variant or associated item named `Out` for `ParamMode`.
- `E0308`: checked parameters now use `Option<CheckedParamMode>`, not
  `ParamMode`.
- `E0599`: no variant named `Lock` for `Statement`.
- `E0599`: no variant named `Merge` for `Statement`.

Representative files:

- `crates/marrow-lsp-core/src/hover.rs`
- `crates/marrow-lsp-core/src/navigation.rs`
- `crates/marrow-lsp-core/src/semantic_tokens.rs`
- `crates/marrow-lsp-core/src/signature_help.rs`
- `crates/marrow-lsp-core/src/store.rs`

Notes: production code should delete `out`, `lock`, and `merge` handling and
render checked `inout` through `CheckedParamMode`.

### Evolve Declaration Exhaustiveness

Representative errors:

- `E0004`: non-exhaustive patterns, `Declaration::Evolve(_)` not covered.

Representative files:

- `crates/marrow-lsp-core/src/hover.rs`
- `crates/marrow-lsp-core/src/navigation.rs`
- `crates/marrow-lsp-core/src/semantic_tokens.rs`
- `crates/marrow-lsp-core/src/symbols.rs`

Notes: `evolve` is current v0.1 source syntax and needs explicit source
presentation handling rather than a wildcard placeholder.

### Run, MCP, And Identity API Alignment

Representative errors:

- `E0061`: `run_entry_with_host` takes 3 arguments, but LSP passes 5.
- `E0599`: no method named `len` on `IdentityValue`.

Representative files:

- `crates/marrow-lsp-core/src/mcp.rs`

Notes: current Marrow expects `run_entry_with_host(&TreeStore, &Host,
&CheckedEntryCall)`. Identity presentation should use `IdentityValue::root()`
and `IdentityValue::keys()`.

## Latent Downstream Failures

These earlier Phase 0 scans look past the first `marrow-lsp-core` compile wall
to identify downstream code and tests that are likely to fail or preserve
rejected production surfaces after core starts compiling. They are retained as
historical discovery evidence, not as a substitute for the final live commands
above.

Summary by area:

- `crates/marrow-dap`: direct downstream compile risk from
  `marrow_store::path`, `marrow_store::backend::Backend`, `SavedPathClass`,
  `classify_saved_path`, and `decode_identity_arity`. It also still handles
  removed `Statement::Lock` and `Statement::Merge` in breakpoint code, threads
  `allowPrototypeArgs` and `allowRawDataInspection`, and exposes a
  debug/admin durable-data prototype surface.
- `crates/marrow-mcp`: catalog and server code still expose raw saved-data
  tools, debug/admin prototype descriptions, and `allowPrototypeArgs`. Tests in
  the same crate assert those descriptions and import private
  `marrow_store::backend` and `marrow_store::path` APIs.
- `crates/marrow-lsp`: the binary remains a thin downstream of
  `marrow-lsp-core`; backend code depends on core `data_explorer`,
  `data_integrity`, and `store` modules. LSP smoke tests directly import
  private `marrow_store::backend` and `marrow_store::path` APIs and exercise
  `marrow/dataIntegrity`.
- Tests: there is no top-level `/Users/scottwilliams/Dev/lane-production-baseline/tests`
  directory. The actual crate test directories contain DAP prototype-value
  assertions, MCP prototype/raw saved-data assertions, and LSP private-store
  setup. Phase 1 should identify each test family after workspace compile
  closure and then delete, replace, or explicitly block them without weakening
  prototype expectations.
- `editors/vscode/package.json`: launch configuration still exposes
  `allowPrototypeArgs`, `allowRawDataInspection`, debug/admin prototype entry
  strings, raw saved-data inspection language, and scripts that check the data
  explorer copy.

## Lockfile And Dependency Notes

The Rust baseline check updated `Cargo.lock` before failing:

- `block-buffer v0.12.0`
- `cpufeatures v0.3.0`
- `crypto-common v0.2.2`
- `digest v0.11.3`
- `hybrid-array v0.4.12`
- `sha2 v0.11.0`
- `typenum v1.20.1`

It also added `sha2` dependency edges for the path-dependent Marrow crates in
the lane lockfile. This was recorded with
`git -C /Users/scottwilliams/Dev/lane-production-baseline diff -- Cargo.lock`
and restored with
`git -C /Users/scottwilliams/Dev/lane-production-baseline restore -- Cargo.lock`.
The final requested `Cargo.lock` status command produced no output.

## Recommended Phase 1 Sublane Order

1. Run Phase 1A, Public Store And Tooling Boundary, first. It owns the largest
   failure cluster, removes private `marrow_store` and deleted saved-path
   classifier dependencies, and should include sibling cleanup for the
   `data_explorer.rs` key-shape errors.
2. Run a serialized source-facts stack next because the files overlap:
   Phase 1C, Resolver And Alias API Alignment, then Phase 1B, Syntax Surface
   Alignment and `Declaration::Evolve` source presentation.
3. Run Phase 1D, Run, MCP, And DAP Entry API Alignment, after the store
   boundary has settled, since `mcp.rs`, downstream MCP, and downstream DAP
   currently have both store-boundary and run-entry failures.
4. Run Phase 1E, Workspace Closure And Test Family Review, only after the
   compile wall moves past `marrow-lsp-core`: intentionally review any lockfile
   delta, close the full workspace compile across `marrow-lsp-core`,
   `marrow-lsp`, `marrow-mcp`, and `marrow-dap`, then identify the affected
   test families without weakening prototype tests.

No source code was changed during Phase 0. The intended final dirty state is
this tracker document only.
