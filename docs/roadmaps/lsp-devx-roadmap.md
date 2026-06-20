# Marrow LSP And VS Code Dev Experience Roadmap

This roadmap guides feature direction for the Marrow LSP and VS Code developer
experience. Active hardening work now runs from
[`production-readiness-roadmap.md`](production-readiness-roadmap.md). Feature
lanes below resume only when they are consistent with that roadmap and the fact
ledger.

During the Marrow prototype-to-v0.1 rewrite, use
[`v01-parallel-orchestration.md`](v01-parallel-orchestration.md) as historical
coordination context and
[`lsp-fact-consumption-ledger.md`](lsp-fact-consumption-ledger.md) as the
surface-status contract.

## Operating Rules

- Work in short-lived, file-disjoint worktrees outside the tracked repo, with each lane using its
  own external build-output directory. Place LSP worktrees directly under
  `/Users/scottwilliams/Dev/` so the workspace path dependency `../marrow` still resolves to the
  live Marrow checkout.
- Keep lanes small enough to review adversarially and merge often.
- Before every integration, re-check `main`, rebase the lane onto current `main`, fast-forward only
  when conflicts are mechanical, and preserve user-owned commits and dirty work.
- Treat `/Users/scottwilliams/Dev/marrow/docs/language/` as the source of truth for language
  behavior. When Marrow language agents change semantics, adapt the LSP through canonical Marrow
  crate APIs rather than reimplementing behavior in the LSP repo.
- Treat the live `/Users/scottwilliams/Dev/marrow` checkout as read-only for LSP lanes. If an
  editor feature needs new language facts or checker APIs, stop the LSP lane and record a
  `blocked-on-marrow` handoff for the Marrow project lead.
- Prefer upstream Marrow APIs for missing facts. The Marrow project lead owns any needed
  `marrow-check`, `marrow-schema`, or `marrow-syntax` API lane; the LSP orchestrator adapts after
  that upstream API is integrated.
- Every behavior lane starts with failing tests, then minimal implementation, then refactor under
  green.
- Every lane gets at least two read-only reviews before integration: one soundness review that tries
  to break the change with repros, and one idiom/spec review that checks fit with Marrow semantics
  and repo style.
- Cleanup is continuous but scoped: improve files touched by a lane, remove stale helpers,
  duplicated logic, prototype paths, compatibility shims, and low-value comments, and do not
  bundle unrelated refactors.

## Integration Bar

Each integrated lane must pass fresh checks from the current `main` tip:

- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all --check`
- `npm --prefix editors/vscode run check`
- `npm --prefix editors/vscode run compile`
- no `unsafe`
- no new dependencies unless the user approves a specific need

Use narrower checks first inside a lane, then run the full bar before merge.

## Dependency Watch

At the start of each lane:

1. Check `git -C /Users/scottwilliams/Dev/marrow status --short --branch`.
2. Check recent Marrow language commits with `git -C /Users/scottwilliams/Dev/marrow log --oneline -8`.
3. Re-read only the relevant files under `/Users/scottwilliams/Dev/marrow/docs/language/`.
4. Confirm whether the needed facts already exist in `marrow-check`, `marrow-schema`,
   `marrow-syntax`, or `marrow-run`.

If upstream Marrow changes break or obsolete an LSP lane, stop that lane, record the impact, rebase
or redesign against the new canonical API, and avoid patching around it in TypeScript or regex.

Current watch items:

- Decide upstream in Marrow whether identity decoding should reject non-canonical bool key bytes.
  The LSP must keep following `marrow-run`'s canonical identity decoder rather than becoming
  stricter on its own.

## Roadmap

### Lane 1: Symbol Facts Backbone

Goal: expose one project-wide source of symbol facts for coloring, hover, navigation, completion,
and rename.

Expected outcomes:

- Extend the binding/index surface to cover enums, enum members, builtin functions, standard
  library modules and functions, resource roots, resource identity keys, function parameters, and
  resource members.
- Add binding coverage for enum type names in annotations, including qualified and imported enum
  types, so hover and navigation are not limited to enum values.
- Preserve existing navigation and rename safety for saved-data-backed symbols.
- Add focused tests for enum value jump-to-definition, function call facts, resource root facts,
  builtin facts, and parameter symbol identity.
- Keep the API in Rust core; TypeScript remains a launcher only.

Likely LSP files:

- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/hover.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/navigation.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/semantic_tokens.rs`

Upstream candidates, only in a separate Marrow worktree:

- `/Users/scottwilliams/Dev/marrow/crates/marrow-check/src/binding.rs`

### Lane 2: Rich Semantic Tokens

Goal: make text coloring resemble established LSP-backed editors: tokens reflect resolved roles, not
just lexer categories.

Expected outcomes:

- Color modules as `namespace`.
- Color functions as `function`.
- Color resources as `struct`, generated identities as `class` or `type`, enums as `enum`, enum
  values as `enumMember`, fields/layers/indexes as `property`, and parameters as `parameter`.
- Mark builtins and standard-library symbols with `defaultLibrary`.
- Split operators, literals, builtins, and saved roots enough that common themes can distinguish
  them.
- Add VS Code semantic token contribution mappings for custom Marrow token types such as saved
  roots when standard token types do not fit.

Likely LSP files:

- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/semantic_tokens.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp/src/backend.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/editors/vscode/package.json`
- `/Users/scottwilliams/Dev/marrow-lsp/editors/vscode/syntaxes/marrow.tmLanguage.json`

### Lane 3: Hover That Explains The Symbol

Goal: hovering a meaningful name should show the same facts a developer expects in Rust, TypeScript,
or Python tooling: kind, signature/type, docs, and relevant storage context.

Expected outcomes:

- Function hover shows signature, parameter names and modes, parameter docs when available, return
  type, direct checked effect summary, storage/runtime context only where canonical facts exist, and
  `;;` docs.
- Resource hover shows resource name, saved root, identity key names and types, concise member
  summary, docs, and a bounded note when the resource tree is large.
- Resource root hover works on both `^root` sigil/name and root uses.
- Enum and enum-member hover shows enum type, full member path, category/selectable status, and
  docs without presenting source-order ordinals as stable identity.
- Builtin and standard-library hover shows signature and documentation sourced from canonical
  descriptor tables or language docs.
- Parameter hover distinguishes parameters from locals and shows parameter mode plus type.

Likely LSP files:

- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/hover.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/types.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp/src/backend.rs`

Upstream candidates, only in a separate Marrow worktree:

- `/Users/scottwilliams/Dev/marrow/crates/marrow-schema/src/stdlib.rs`

### Lane 4: Navigation Coverage

Goal: jump-to-definition and references should work for all resolved symbols users naturally try.

Expected outcomes:

- Enum type names and enum values jump to declarations.
- Enum type annotations jump through Marrow's binding-index facts where those facts exist; missing
  module-path identity facts remain blocked instead of reconstructed locally.
- Resource names, roots, identities, fields, layers, indexes, and identity keys jump to the
  canonical declaration span.
- Module path segments jump to module declarations or imported module files only after Marrow
  exposes canonical module-path facts.
- Builtins either jump nowhere by design with hover documentation, or jump to canonical docs if the
  project later exposes doc locations.
- Existing saved-data rename refusals remain intact and are expanded to any newly indexed
  saved-data-backed symbols.

Likely LSP files:

- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/navigation.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp/tests/smoke.rs`

Upstream candidates, only in a separate Marrow worktree:

- `/Users/scottwilliams/Dev/marrow/crates/marrow-check/src/binding.rs`

### Lane 5: Completion And Signature Help

Goal: calls and constructors should guide the user while typing, not after diagnostics.

Expected outcomes:

- Keep LSP signature help fact-backed as Marrow exposes more callable context. User functions,
  resource constructors, generated identity constructors, builtins, and standard-library calls are
  already exposed through core signature help.
- Enrich completion items with docs, parameter snippets where helpful, and correct symbol kinds.
- Keep completion context lexer-tolerant so malformed in-progress source still gets useful
  suggestions.

Likely LSP files:

- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/completion.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/signature_help.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp/src/backend.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/editors/vscode/package.json`

### Lane 6: Resource-Aware Dev Experience

Goal: saved resources should be understandable without flooding the editor.

Expected outcomes:

- Resource hover and the Saved Resource Inspector share bounded summary helpers for fields,
  layers, indexes, and key types.
- Large resource trees are summarized with counts and first few members rather than dumping the
  whole tree.
- Live record counts remain blocked until Marrow exposes checked saved-place
  facts, typed store summaries, and source/store generation boundaries.
- Saved Resource Inspector terminology matches hover and completion terminology.

Likely LSP files:

- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/hover.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/data_explorer.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/store.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/editors/vscode/src/savedResourceInspector.ts`

### Lane 7: Cleanup And Drift Hardening

Goal: reduce duplicated presentation logic and make future Marrow language changes cheaper to
absorb.

Expected outcomes:

- Centralize signature rendering for functions, builtins, standard-library functions, resources,
  identities, and enum members.
- Centralize bounded Markdown rendering for hover and completion documentation.
- Add small regression fixtures that exercise enums, resources, builtins, modules, and saved roots
  together.
- Keep the LSP core transport-neutral so MCP can reuse facts where useful.

Likely LSP files:

- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/types.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/hover.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/crates/marrow-lsp-core/src/completion.rs`
- `/Users/scottwilliams/Dev/marrow-lsp/fixtures/shelf/src/shelf/sample.mw`

## Merge Cadence

- Prefer one lane commit per reviewed capability slice.
- Merge to `main` after each clean lane rather than accumulating a long-running feature branch.
- Push after the full integration bar passes on `main`.
- Retire each worktree immediately after the merge and push.

## Backlog Discipline

- Delete completed roadmap checklist items from active task lists.
- Record deferred real findings as new concrete backlog entries.
- Do not keep historical progress notes in source files or docs. The roadmap should describe the
  next useful work, not preserve sediment.
