# marrow-lsp

Editor and agent toolchain for the [Marrow](https://github.com/scottswilliams/marrow) `.mw`
language: a VSCode extension, a Language Server (LSP), an MCP server for coding agents, and a
DAP debugger. They are three transports over one Rust brain.

All language intelligence lives in `marrow-lsp-core`, which reuses the canonical marrow crates
(`marrow-syntax`, `marrow-check`, `marrow-schema`, `marrow-store`, `marrow-run`,
`marrow-project`) and never reimplements the parser, checker, or type system. The marrow repo's
`docs/language/` is the source of truth for `.mw` behavior.

## Start Here

- [Install](docs/install.md) explains source builds, local VSIX packaging, and editor install paths.
- [Quickstart](docs/quickstart.md) walks through opening a Marrow project, diagnostics,
  formatting, saved-data inspection, and F5 debugging.
- [Stability](docs/stability.md) records the editor/tooling support contract and current
  pre-release boundaries.
- [VSCode extension README](editors/vscode/README.md) documents extension-specific settings and
  feature details.
- [Changelog](CHANGELOG.md) tracks repo-level release notes and links to extension-specific notes.

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

The workspace contains the shared Rust core, the LSP/MCP/DAP transports, and the VSCode
extension. Diagnostics, completion, hover, navigation, semantic tokens, formatting, data
inspection, MCP tools, and statement-level debugging are implemented, with language behavior
coming from the canonical marrow crates. See `AGENTS.md` for working conventions.
