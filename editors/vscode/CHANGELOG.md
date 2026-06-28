# Changelog

All notable changes to the Marrow VSCode extension are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0

Initial release.

### Added

- Diagnostics (parse, type, and schema errors) for `.mw` files.
- Source-intelligence helpers over current Marrow analysis: diagnostics, formatting, document and
  workspace symbols, semantic tokens, signature help, and completion are backed by Marrow facts.
  Catalog-backed go-to definition and find references are backed by Marrow navigation facts for
  saved roots, resource catalog leaves, enum leaves, type annotations, members, and indexes.
  Source-only prepare rename and editor rename edits are backed by Marrow rename facts. Hover and
  remaining navigation helpers stay editor aids while saved-data-backed rename application and
  remaining hover/navigation facts stay owned by Marrow.
- Rename supports Marrow-admitted source-only bindings and refuses saved-data-backed edits until
  Marrow's evolve rename intent has a canonical editor application contract.
- Document formatting.
- The Saved Resource Inspector view for the opt-in production read-only data-view contract over
  committed saved resources through Marrow-owned store facts. It automatically refreshes from
  LSP-supplied native dev-store and committed-lock watch targets when `marrow.liveData` is enabled,
  refuses to mix child pages or value reads from different Marrow store snapshots, and does not
  watch uncommitted writes, debuggee state, or served programs.
- F5 debugging via the `marrow-dap` adapter, with explicit `entry`, Marrow-admitted typed `args`
  where `int`, `date`, and `bytes` use exact string forms, stop-on-entry, statement stepping,
  verified plain-line and conditional breakpoints, exact hit-count breakpoints, logpoints with
  Marrow-checked template expressions, watch/REPL/hover expression evaluation, variables, and the
  Durable Data scope. Launch responses carry Marrow's admitted serve-surface boundary with process
  control marked not exposed; attach configurations remain absent until served-process control
  boundary facts exist.
- Per-platform release `.vsix` packages bundling the `marrow-lsp` server and `marrow-dap`
  debug adapter (`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `win32-x64`).
