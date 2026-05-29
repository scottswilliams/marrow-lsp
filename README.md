# marrow-lsp

Editor and agent toolchain for the [Marrow](https://github.com/scottswilliams/marrow) `.mw`
language: a VSCode extension, a Language Server (LSP), an MCP server for coding agents, and a
DAP debugger. They are three transports over one Rust brain.

All language intelligence lives in `marrow-lsp-core`, which reuses the canonical marrow crates
(`marrow-syntax`, `marrow-check`, `marrow-schema`, `marrow-store`, `marrow-run`,
`marrow-project`) and never reimplements the parser, checker, or type system. The marrow repo's
`docs/language/` is the source of truth for `.mw` behavior.

## Layout

```
crates/
  marrow-lsp-core/   the transport-free language intelligence
  marrow-lsp/        LSP server over stdio (thin transport)
  marrow-mcp/        MCP server over stdio (thin transport)
  marrow-dap/        DAP debug adapter (thin transport)
editors/vscode/      VSCode extension: launcher + grammar + views + debug wiring
fixtures/shelf/      a runnable Marrow project used by tests and manual checks
```

## Status

Early. The workspace builds and `marrow-lsp-core::positions` (byte ↔ UTF-16) is in place.
Diagnostics, completion, hover, navigation, the data-inspection features, the MCP server, and
the debugger land in subsequent milestones. See `AGENTS.md` for working conventions.
