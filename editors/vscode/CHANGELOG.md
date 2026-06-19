# Changelog

All notable changes to the Marrow VSCode extension are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0

Initial release.

### Added

- Diagnostics (parse, type, and schema errors) for `.mw` files.
- Presentation-only source-intelligence helpers over current Marrow analysis: hover, completion,
  document and workspace symbols, go-to definition, find references, rename, and semantic tokens.
  These are editor aids, not stable production semantic contracts.
- Rename refuses saved-data-backed edits until Marrow exposes catalog-backed evolution facts.
- Document formatting.
- The Saved Resource Inspector view for opt-in committed saved-resource inspection through
  Marrow-owned store facts.
- F5 debugging via the `marrow-dap` adapter, with explicit `entry`, Marrow-admitted typed `args`
  where `int`, `date`, and `bytes` use exact string forms, stop-on-entry, statement stepping,
  advisory breakpoints, and variables. Durable-data debugger inspection and line breakpoint arming
  remain blocked until Marrow exposes the canonical facts behind those surfaces.
- Per-platform release `.vsix` packages bundling the `marrow-lsp` server and `marrow-dap`
  debug adapter (`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `win32-x64`).
