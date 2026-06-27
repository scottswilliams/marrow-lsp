# Marrow for VSCode

Language support for the Marrow `.mw` language. The extension is a thin launcher: it ships
the TextMate grammar, the editor configuration, the saved-resource view, and the debug wiring,
and it starts the bundled `marrow-lsp` language server and `marrow-dap` debug adapter. All
language intelligence — parsing, checking, typing, schema, and store access — lives in the
Rust binaries. No parsing or position math runs in TypeScript.

## Features

- **Diagnostics** — parse, type, and schema errors reported as you edit.
- **Source intelligence** — diagnostics, formatting, symbols, semantic tokens, signature help,
  and completion are backed by Marrow facts over the current analysis. Hover, go-to definition,
  find references, and rename remain editor aids where the LSP contract says so; durable identity,
  catalog evolution, and typed hover/navigation facts stay owned by Marrow.
- **Rename** — editor rename for supported source bindings; saved-data-backed edits are refused
  until Marrow exposes catalog-backed evolution facts.
- **Formatting** — format a `.mw` document.
- **Saved Resource Inspector** — an opt-in view (Activity Bar) that expands committed saved
  resources from the native dev store when `marrow.liveData` is enabled. It is gated on Marrow
  tooling facts, carries Marrow store snapshot metadata through expansions, refuses to mix child
  pages or value reads from a different snapshot, and automatically refreshes from LSP-supplied
  native dev-store and committed-lock watch targets when `marrow.liveData` is enabled. The view
  title remains available for manual refreshes. It does not accept editor-authored saved paths.
- **Debugging (F5)** — statement-level debugging through the `marrow-dap` adapter: launch a
  project's `defaultEntry`, or set `entry` and typed `args` in `launch.json`. Marrow admits
  the args; `int`, `date`, and `bytes` use exact string forms. F5 remains useful for launch,
  stop-on-entry, stepping, verified plain-line and conditional breakpoints, exact hit-count
  breakpoints, logpoints with Marrow-checked template expressions, watch/REPL/hover expression
  evaluation, variables, and the Durable Data scope.

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
- `marrow.liveData` — opt in to read-only native dev-store reads for the Saved Resource Inspector
  and advisory data-integrity checks. Leave disabled to prevent editor-triggered reads of local
  stored data.

## Known limitations

- The bundled binaries are **platform-specific**. Each release `.vsix` targets one platform
  (`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`, `win32-x64`); install the one that
  matches your machine, or point `marrow.server.path` / `marrow.dap.path` at your own build.
- The Saved Resource Inspector only auto-refreshes LSP-supplied native dev-store and committed-lock
  watch targets when `marrow.liveData` is enabled. It does not watch uncommitted writes, live
  debuggee state, or served programs; a disabled, busy, or missing store is treated as
  "unavailable" rather than reporting an error.
- Debugging is **statement-level** (not expression-level stepping).

## Development

```sh
npm ci
npm run compile   # esbuild bundle to out/extension.js
npm run check     # tsc --noEmit typecheck
npm run bundle    # requires CARGO_TARGET_DIR in the environment
npm run package   # requires CARGO_TARGET_DIR in the environment
```

Set `CARGO_TARGET_DIR` in your environment before running `npm run bundle` or `npm run package`.
It must point at a build-output directory outside this repo so package smoke never writes Rust
build output into the source checkout. `npm run package` computes the VSCode platform target
with Node and validates unsupported local platforms before invoking `vsce`.

## Installing the extension (developers)

Two workflows, depending on whether you are hacking on the extension or just want a build of it
installed in your editor.

### Iterating on the extension — Extension Development Host

Use this while editing the extension's TypeScript or grammar. It runs the extension from source
against a debug build of the binaries, with no packaging step.

```sh
# from the repo root: build the debug binaries the dev fallback resolves at target/debug/
cargo build --manifest-path Cargo.toml -p marrow-lsp -p marrow-dap

# from editors/vscode: install deps and bundle the extension once
npm ci
npm run compile
```

Then open the `editors/vscode` folder in VSCode and press **F5** ("Run Extension"). This launches
an Extension Development Host window with the extension loaded; it finds the binaries via the
`target/debug/<binary>` dev fallback. Re-run `npm run compile` (or pass `--watch` to esbuild) and
reload the host window to pick up extension changes; re-run `cargo build` to pick up binary
changes.

### Installing a packaged build into your editor

Use this to run a real, self-contained `.vsix` in your day-to-day VSCode — it bundles the release
binaries, so it does not depend on a source checkout.

```sh
# from editors/vscode, with CARGO_TARGET_DIR pointing outside the repo
export CARGO_TARGET_DIR=/path/outside/repo/target
npm ci
npm run package   # builds release binaries, bundles them, and emits marrow-<target>-<version>.vsix

code --install-extension marrow-<target>-<version>.vsix
```

Reload or restart VSCode after installing. The `.vsix` filename includes the platform target
(e.g. `darwin-arm64`) and the version from `package.json`.

### Updating an installed build

Repackage and reinstall over the existing extension, then reload:

```sh
npm run package
code --install-extension --force marrow-<target>-<version>.vsix
```

`--force` replaces the currently installed version. If you rebuilt without bumping `version` in
`package.json`, bump it or fully uninstall first
(`code --uninstall-extension scottswilliams.marrow`) so VSCode does not keep the old copy cached.
Run **Developer: Reload Window** (or restart VSCode) to load the new build.

See `CHANGELOG.md` for release notes.
