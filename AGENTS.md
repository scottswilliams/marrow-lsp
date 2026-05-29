# Agent Instructions

This repository is **marrow-lsp** — the editor and agent toolchain for the Marrow `.mw`
language: a VSCode extension, a Language Server (LSP), an MCP server for coding agents, and a
DAP debugger. They are three transports over one Rust brain (`marrow-lsp-core`).

`docs/language/` **in the marrow repo** is the source of truth for `.mw` behavior. This
repository never redefines language semantics; it reuses the canonical marrow crates and
surfaces them through editors and agents.

<!-- SHARED ENGINEERING CORE — canonical copy in marrow/AGENTS.md; keep byte-identical across marrow and marrow-lsp. No repo-specific details (build tools, language surface, paths) belong in this block. -->
## Shared Engineering Core

### Think Before Coding
State assumptions, surface tradeoffs, and push back when a simpler approach exists. Ask rather
than guess when guessing would risk the work.

### Simplicity First
Write the minimum code that solves the problem. Avoid speculative features, single-use
abstractions, and unrequested configurability. The best code is the code you did not write.

### Surgical Changes
Touch only what the request requires. Match the surrounding style. Every changed line should
trace directly to the request. Do not bundle unrelated reverts or refactors.

### Test-Driven Development
Behavior changes start from a failing check, never from implementation. Write or identify the
failing test first, watch it fail, make it pass with the minimum code, then refactor under
green. Keep tests next to the behavior they describe.

### The 80/20 Rule
Prefer changes whose impact is proportionate to their size. Avoid large rewrites that do not
earn their cost.

### Anti-Slop & No Documentation Sediment
No agentic slop in code or docs. Delete stale content rather than shuffle it; do not preserve
obsolete names, formats, examples, or transition shims for their own sake. When the design
changes, clean as if the new design had always been here.

### Comments Explain Why
Write comments as a human engineer would: plain prose explaining rationale. Do not cite files
by name or line, reference tickets, reviews, roadmap steps, or wave numbers, narrate edits
("previously", "now changed to"), or restate what the code already says.

### Worktree Discipline
Use an isolated worktree for multi-file changes, compiled-language changes, or cleanup batches.
Keep harness files, throwaway worktrees, build outputs, trial artifacts, patches, reviews,
logs, and leases outside the tracked repository. The tracked repo holds source, tests, and
durable reference docs only.

### Verification Before Completion
Run focused checks before broad ones. Never claim completion without fresh command output. Do
not run broad build or test gates in parallel against one shared build-output directory.

### Review Before Integrate
Do not merge feature work into the default branch without review. Use short-lived branches,
small commits, focused verification, and a read-only high-reasoning review before integration.
Prefer cherry-picking a reviewed commit over merging a whole branch; if a conflict is not an
obvious mechanical one, send the branch back rather than forcing it.
<!-- END SHARED ENGINEERING CORE -->

## marrow-lsp-Specific Rules

### Three Transports, One Core
All language intelligence lives in `marrow-lsp-core`. `marrow-lsp` (LSP/stdio), `marrow-mcp`
(MCP/stdio), and `marrow-dap` (DAP) are thin transports over it. A new capability lands in the
core first, then each transport exposes it. No transport carries logic another would duplicate.

### Never Reimplement Language Logic
Parsing, checking, typing, schema, store access, and execution come only from the canonical
marrow crates. Zero reimplementation — a TypeScript or hand-rolled re-parse drifts from
`docs/language/`. If something is missing upstream, the fix is a small, opt-in addition in
marrow, not a fork in the core.

### Dependency on the marrow Crates
Depend on the marrow crates via a path dependency during local development, and a single pinned
git revision once marrow is published. `marrow-lsp-core` is the only crate that links marrow.
Track upstream changes deliberately: pin and bump on purpose, never implicitly.

### DAP & Runtime-Hook Discipline
The debugger drives a minimal, opt-in step hook in the marrow runtime, gated so normal
execution is unaffected and carries no overhead. Read-only store access opens the store
read-only and treats a busy or unreadable store as a soft "unavailable" state, never an error
shown to the user.

### Dual Rust + TypeScript Stack
A Rust workspace (one core library plus thin transport binaries) and a thin TypeScript VSCode
extension that bundles the prebuilt binaries, the grammar, the views, and the debug wiring. The
extension is a launcher, not a second application: no parsing, position math, or project
interpretation in TypeScript. No Rust use of `unsafe`.

### Anti-Bloat Boundaries
Smallest thing that is genuinely great, then iterate. No feature in a transport that is not
backed by the core. No vendored copy of marrow types. No speculative surface beyond what an
editor or agent actually uses. Respect marrow's own non-goals — tooling must not smuggle in
capabilities the kernel deliberately excludes.

### Worktree & Build Harness
Keep build outputs, throwaway worktrees, and package-manager artifacts outside the tracked tree
where practical, under this repo's own harness root (separate from marrow's). Set an external
build-output directory per lane so parallel work does not contend over one target directory.

### Verification Ladder
- Rust: the smallest crate or test target that proves the change, then the whole workspace for
  broad changes, with lints and formatting clean and no `unsafe`.
- TypeScript: typecheck and lint, then extension tests, then a packaged-extension smoke test.
- Cross-stack: the extension launches the bundled binary and exercises one real request per
  transport.
