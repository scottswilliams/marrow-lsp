# marrow-lsp agent instructions

This repository holds the standalone Marrow editor tooling: a VS Code extension,
an LSP server, an optional MCP server, and a DAP debugger, built against a pinned
Marrow revision. It is a tooling client of the Marrow language; it does not
define it.

## Status and direction

The active Marrow campaign refounds editor support as a minimal semantic
language server inside the Marrow repository. This repository is slated to be
preserved and frozen once that in-tree server ships its first installed
artifact; until then it remains usable against its pinned revision. Do not start
new semantic tooling capabilities here, bump the Marrow pin speculatively, or
extend the MCP/DAP surface. Work that needs a new language fact adds it in the
Marrow repository first.

## Authority

Use the Marrow repository in this order:

1. `docs/language/` for current language behavior;
2. `docs/vision.md` for product direction and non-goals;
3. `docs/status.md` for the boundary between implemented, legacy, and future work;
4. `docs/future/` for unimplemented direction;
5. `docs/implementation/` and the canonical Marrow crates for implementation facts.

There is no `docs/design/` tier, ADR archive, or target-contract queue. Label
claims **current**, **legacy**, **future**, or **research**. If prose and code
disagree about current behavior, establish the discrepancy with a minimal
fixture and repair the canonical Marrow source; do not create a local
interpretation in marrow-lsp.

## Boundaries

Language intelligence is concentrated in `marrow-lsp-core`; the transports
expose parts of it. Transports own protocol conversion, cancellation, lifecycle,
and rendering; they do not own language semantics. The tooling layer may select,
cache, version, and present semantic facts. It must not reparse or reconstruct
types, language constructs, stable identity, typed semantic paths, authority
requirements, durable effects, evolution verdicts, or runtime and storage
meaning. Rendered diagnostic text is output, not a semantic API.

Rust owns analysis, semantic state, runtime integration, and transport servers.
The TypeScript VS Code extension bundles and launches binaries, contributes the
grammar and UI, and adapts editor APIs; it does not parse Marrow, calculate
semantic positions independently, interpret projects, or reproduce diagnostics.
Use typed IDs, versioned snapshots, typed facts, and structured errors. No Rust
`unsafe`.

## Working method

Maintenance changes only. Follow the parent workspace instructions for
worktrees, mandatory lane-local Cargo target directories, review, and
integration; keep build output, packaged extensions, temporary fixtures, and
probe logs outside the tracked repository.

Start a behavior change with a failing production-path check. A code
integration requires a full Rust workspace build, full workspace tests,
`fmt --check`, `clippy -D warnings`, and zero `unsafe`; run TypeScript
typecheck/lint and extension tests in proportion to the affected layers. Do not
add a fallback parser, vendored Marrow type, compatibility branch, or
speculative transport feature to preserve an obsolete test.
