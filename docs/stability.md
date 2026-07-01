# Stability

marrow-lsp is pre-release tooling for the Marrow `.mw` ecosystem. It aims to expose editor and
agent workflows over canonical Marrow facts without becoming a second language implementation.

## Source of truth

- The Marrow repository owns `.mw` syntax, checking, schema, store, runtime, debugger, and durable
  data semantics.
- marrow-lsp consumes Marrow crates and facts through `marrow-lsp-core`.
- LSP, MCP, DAP, and VSCode are transport and presentation surfaces.

This repository may describe how a feature appears in an editor, but it should not define language
semantics.

## Release dependency

Local development uses path dependencies on the Marrow crates. A final release should pin one
reviewed Marrow revision for the release line. Until that pin is named by the release, treat source
checkouts and locally packaged VSIX files as pre-release artifacts.

## Platform packages

VSCode packages are platform-specific because they bundle native `marrow-lsp` and `marrow-dap`
binaries. The packaging scripts currently target:

- `darwin-arm64`
- `darwin-x64`
- `linux-x64`
- `linux-arm64`
- `win32-x64`

Install a package that matches your platform, or set `marrow.server.path` and `marrow.dap.path` to
your own binaries.

## Rust APIs

Rust crate APIs in this workspace are not documented as stable public APIs. Prefer using the
transport binaries or the VSCode extension unless you are working on marrow-lsp itself.

## Extension settings

The supported extension settings are:

- `marrow.server.path` for an absolute `marrow-lsp` override.
- `marrow.dap.path` for an absolute `marrow-dap` override.
- `marrow.liveData` to opt in to read-only saved-resource inspection.

Settings may change before a stable release. See [editors/vscode/README.md](../editors/vscode/README.md)
for current behavior.

## Current boundaries

- Diagnostics, source intelligence, formatting, saved-resource inspection, MCP tools, and
  standalone statement-level debugging are implemented over Marrow-owned facts.
- Saved-data-backed editor rename application is not exposed until Marrow provides a canonical
  editor application contract.
- Served-process attach/debug launch is not advertised until Marrow exposes served-process control
  boundary facts.
- Data integrity and repair UI is absent until Marrow exposes production validation and repair
  facts.
