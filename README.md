# marrow-lsp

Editor and agent toolchain for the [Marrow](https://github.com/scottswilliams/marrow) `.mw`
language: a VSCode extension, a Language Server (LSP), an MCP server for coding agents, and a
DAP debugger. They are three transports over one Rust brain.

All language intelligence lives in `marrow-lsp-core`, which reuses the canonical marrow crates
(`marrow-syntax`, `marrow-check`, `marrow-catalog`, `marrow-json`, `marrow-schema`,
`marrow-store`, `marrow-run`, `marrow-project`) and never reimplements the parser, checker, or
type system. The marrow repo's `docs/language/` is the source of truth for `.mw` behavior.

## Layout

```
crates/
  marrow-lsp-core/   the transport-free language intelligence
  marrow-lsp/        LSP server over stdio (thin transport)
  marrow-mcp/        MCP server over stdio (thin transport)
  marrow-dap/        DAP debug adapter (thin transport)
  marrow-terminal/   terminal styling helpers shared by the transport binaries
editors/vscode/      VSCode extension: launcher + grammar + views + debug wiring
fixtures/shelf/      a runnable Marrow project used by tests and manual checks
xtask/               workspace maintenance (consumed-marrow-surface drift snapshot)
```

## Status

The workspace contains the shared Rust core, the LSP/MCP/DAP transports, and the VSCode
extension. Diagnostics, completion, hover, navigation, semantic tokens, formatting, on-type
indentation, read-only data inspection, MCP tools, and statement-level debugging are
implemented, with language behavior coming from the canonical marrow crates.

Some surfaces are intentionally absent until Marrow exposes the production facts they need:
data integrity and repair, live saved-data hover and record counts, saved-data-backed editor
rename, and attaching to served (running) programs. The
[production readiness roadmap](docs/roadmaps/production-readiness-roadmap.md) is the plan from
the current state to production; the [fact-consumption ledger](docs/roadmaps/lsp-fact-consumption-ledger.md)
records what each surface may treat as semantic authority. See `AGENTS.md` for working
conventions.
