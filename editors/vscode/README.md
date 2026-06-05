# Marrow for VSCode

Language support for the Marrow `.mw` language. The extension is a thin launcher: it ships
the TextMate grammar, the editor configuration, the root-only data view, and the debug wiring,
and it starts the bundled `marrow-lsp` language server and `marrow-dap` debug adapter. All
language intelligence — parsing, checking, typing, schema, and store access — lives in the
Rust binaries. No parsing or position math runs in TypeScript.

## Features

- **Diagnostics** — parse, type, and schema errors reported as you edit.
- **Source intelligence** — presentation-only hover, completion, symbols, go-to definition,
  find references, rename, and semantic tokens over the current Marrow analysis. These are editor
  aids, not stable production semantic contracts; durable identity, catalog evolution, and
  query-native hover/navigation/completion facts stay owned by Marrow.
- **Rename** — editor rename for supported source bindings; saved-data-backed edits are refused
  until Marrow exposes catalog-backed evolution facts.
- **Formatting** — format a `.mw` document.
- **Marrow Data Roots** — an opt-in view (Activity Bar) that lists saved root names from the
  native dev store when `marrow.liveData` is enabled. It is gated on Marrow tooling facts and does
  not accept editor-authored saved paths.
  Refreshes automatically when `marrow.json` is saved, and on demand from the view title.
- **Debugging (F5)** — statement-level debugging through the `marrow-dap` adapter: launch a
  project's `defaultEntry` while Marrow's canonical function-entry facts are pending. F5 remains
  useful for launch, stop-on-entry, stepping, advisory breakpoints, and variables. Explicit
  entry strings, durable-data debugger inspection, and line breakpoint arming are blocked until
  Marrow exposes the canonical facts behind those surfaces.

## Requirements

- **VSCode 1.85+**.
- A **Marrow project**: a directory containing a `marrow.json`. The extension activates on
  `.mw` files and on workspaces that contain a `marrow.json`.

The release `.vsix` for your platform **bundles** the `marrow-lsp` and `marrow-dap` binaries,
so no separate install or Rust toolchain is required.

## How the extension finds the server binary

Both the language server (`marrow-lsp`) and the debug adapter (`marrow-dap`) are resolved the
same way, in order:

1. The explicit setting — `marrow.server.path` for the server, `marrow.dap.path` for the debug
   adapter — when set to an absolute path that exists on disk.
2. The **bundled** binary shipped inside the extension at `server/marrow-lsp` /
   `server/marrow-dap`. This is what a Marketplace/`.vsix` install uses.
3. A local **dev build** at `target/debug/<binary>` relative to the extension checkout, for
   working on the extension from a source tree. This fallback is disabled in packaged installs.

## Settings

- `marrow.server.path` — absolute path to the `marrow-lsp` binary. Overrides the bundled and
  dev locations when the file exists.
- `marrow.dap.path` — absolute path to the `marrow-dap` debug adapter binary. Overrides the
  bundled and dev locations when the file exists.
- `marrow.liveData` — opt in to read-only native dev-store reads for the Marrow Data Roots view and
  advisory data-integrity checks. Leave disabled to prevent editor-triggered reads of local
  stored data.

## Known limitations

- The bundled binaries are **platform-specific**. Each release `.vsix` targets one platform
  (`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `win32-x64`); install the one that
  matches your machine, or point `marrow.server.path` / `marrow.dap.path` at your own build.
- The Marrow Data Roots view lists saved root names only. Child paths, stored values, and paging remain
  blocked on catalog-bound typed data facts. A disabled, busy, or missing store is treated as
  "unavailable" rather than reporting an error.
- Debugging is **statement-level** (not expression-level stepping).

## Development

```sh
npm install
npm run compile   # esbuild bundle to out/extension.js
npm run check     # tsc --noEmit typecheck
npm run bundle    # build the two release binaries into ./server/
npm run package   # minified compile + bundle + vsce package for this platform
```

See `CHANGELOG.md` for release notes.
