# Marrow for VSCode

Language support for the Marrow `.mw` language. This extension is a thin launcher: it ships
the TextMate grammar and editor configuration, and it starts the `marrow-lsp` server, which
provides all language intelligence (diagnostics, hover, navigation, and the rest). No parsing
or language logic lives in this extension.

## Features

- Syntax highlighting for `.mw` files.
- Indentation-aware editing (comments, brackets, on-enter rules).
- Launches and talks to the `marrow-lsp` language server over stdio.

## Server binary

The extension resolves the server in this order:

1. The `marrow.server.path` setting, if set to an absolute path.
2. The bundled binary at `server/marrow-lsp` inside the extension.
3. The local dev build at `target/debug/marrow-lsp` in the workspace checkout.

For local development, build the server in the `marrow-lsp` repo so the dev path resolves.

## Settings

- `marrow.server.path`: absolute path to the `marrow-lsp` binary. Overrides the bundled and
  dev locations.

## Development

```sh
npm install
npm run compile   # esbuild bundle to out/extension.js
npm run check     # tsc --noEmit typecheck
```
