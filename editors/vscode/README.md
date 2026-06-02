# Marrow for VSCode

Language support for the Marrow `.mw` language. The extension is a thin launcher: it ships
the TextMate grammar, the editor configuration, the Data Explorer view, and the debug wiring,
and it starts the bundled `marrow-lsp` language server and `marrow-dap` debug adapter. All
language intelligence — parsing, checking, typing, schema, and store access — lives in the
Rust binaries. No parsing or position math runs in TypeScript.

## Features

- **Diagnostics** — parse, type, and schema errors reported as you edit.
- **Hover** — type and documentation on hover. Live data values are available only when
  `marrow.liveData` is enabled.
- **Completion** — context-aware completion of names in scope.
- **Document & workspace symbols** — outline a single `.mw` file and search symbols across the
  whole project.
- **Go to definition** — jump from a use to its binding.
- **Find references** — list every use of a symbol.
- **Rename** — data-safe rename that refuses edits that would diverge from the persisted store
  schema, so renames never silently break durable data.
- **Semantic tokens** — semantic highlighting layered over the TextMate grammar.
- **Formatting** — format a `.mw` document.
- **CodeLens** — a **record-count** lens above declarations when `marrow.liveData` is enabled.
- **Marrow Data Explorer** — an opt-in debug/admin view (Activity Bar) for inspecting the native
  dev store when `marrow.liveData` is enabled. It is not a production catalog view yet; rows use
  the current raw saved-data protocol while Marrow's typed store DTOs are still pending.
  Refreshes automatically when `marrow.json` is saved, and on demand from the view title.
- **Debugging (F5)** — statement-level debugging through the `marrow-dap` adapter: launch an
  entry function, step, set breakpoints, inspect variables, and read a live `^` durable-data
  scope that surfaces the project's persisted values alongside ordinary locals.

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
- `marrow.liveData` — opt in to read-only native dev-store reads for hover, CodeLens, the Data
  Explorer, and data-integrity checks. Leave disabled to prevent editor-triggered reads of local
  stored data.

## Known limitations

- The bundled binaries are **platform-specific**. Each release `.vsix` targets one platform
  (`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `win32-x64`); install the one that
  matches your machine, or point `marrow.server.path` / `marrow.dap.path` at your own build.
- Live data values, the record-count CodeLens, and the Data Explorer are opt-in debug/admin
  surfaces. They require `marrow.liveData` and a readable native dev store, and remain
  presentation-only until Marrow exposes catalog-bound typed data facts. A disabled, busy, or
  missing store is treated as "unavailable" and simply omits live values rather than reporting an
  error.
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
