# Changelog

All notable changes to the Marrow VSCode extension are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.1.0

Initial release.

### Added

- Diagnostics (parse, type, and schema errors) for `.mw` files.
- Hover with type and documentation, plus opt-in debug/admin live data presentation from the
  native dev store while typed/catalog-bound facts are pending.
- Context-aware completion.
- Document and workspace symbols.
- Go to definition and find references.
- Data-safe rename that refuses saved-data-backed edits.
- Semantic tokens for semantic highlighting over the TextMate grammar.
- Document formatting.
- Presentation-only record-count CodeLens when opt-in live data is enabled.
- The Marrow Data Explorer view for opt-in debug/admin inspection of the native dev store while
  typed/catalog-bound store facts are pending.
- F5 debugging via the `marrow-dap` adapter, with statement stepping, breakpoints, and a live
  `^` durable-data scope.
- Per-platform release `.vsix` packages bundling the `marrow-lsp` server and `marrow-dap`
  debug adapter (`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `win32-x64`).
