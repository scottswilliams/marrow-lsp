# Install marrow-lsp

marrow-lsp is currently documented for source builds and locally packaged VSIX installs. A public
VSIX distribution channel is not assumed here; when a release artifact exists, use the artifact and
platform target named by that release.

Marrow language semantics are defined in the Marrow repository. This repository supplies editor,
LSP, MCP, and DAP tooling over the canonical Marrow crates.

## Requirements

- Rust toolchain matching `rust-toolchain.toml`.
- Node.js and npm for the VSCode extension package scripts.
- VSCode 1.85 or newer for the extension.
- A sibling Marrow source checkout at `../marrow`; this workspace depends on
  `../marrow/crates/*` while Marrow crates are unpublished.
- A Marrow project directory containing `marrow.json`.

## Source checkout layout

Source builds require the Marrow repository next to this repository because the workspace uses
relative path dependencies into the canonical Marrow crates:

```text
workspace/
  marrow/
  marrow-lsp/
```

For a fresh source setup:

```sh
mkdir marrow-workspace
cd marrow-workspace
git clone https://github.com/scottswilliams/marrow.git
git clone https://github.com/scottswilliams/marrow-lsp.git
cd marrow-lsp
```

If this checkout has a different directory name, the required part is still that its parent
directory contains a sibling named `marrow`.

## Build the Rust tools from source

From the repository root, build the transport binaries:

```sh
CARGO_TARGET_DIR=/path/outside/repo/target cargo build --manifest-path Cargo.toml \
  -p marrow-lsp -p marrow-mcp -p marrow-dap
```

Use an output directory outside the source checkout. The resulting debug binaries are under the
target directory you chose.

## Run the VSCode extension from source

Use this path when developing the extension or testing a checkout directly:

```sh
CARGO_TARGET_DIR=/path/outside/repo/target cargo build --manifest-path Cargo.toml \
  -p marrow-lsp -p marrow-dap
npm --prefix editors/vscode ci
npm --prefix editors/vscode run compile
```

Open `editors/vscode` in VSCode and press F5 to launch an Extension Development Host. The
development host resolves `marrow-lsp` and `marrow-dap` from the local debug build fallback.

## Package a local VSIX

Use this path when you want to install a self-contained extension build into VSCode:

```sh
npm --prefix editors/vscode ci
CARGO_TARGET_DIR=/path/outside/repo/target npm --prefix editors/vscode run package
```

The package script builds release binaries, bundles them into the extension, computes the local
VSCode platform target, and emits a `marrow-<target>-<version>.vsix` file from the extension
package directory.

Install the generated VSIX with:

```sh
code --install-extension editors/vscode/marrow-<target>-<version>.vsix
```

If a release publishes a VSIX artifact, install the artifact that matches your platform target
instead of building it locally.

## Configure explicit binary paths

The VSCode extension normally uses bundled binaries in packaged installs and local debug binaries
in Extension Development Host sessions. You can override both with absolute paths:

- `marrow.server.path` points to `marrow-lsp`.
- `marrow.dap.path` points to `marrow-dap`.

See [editors/vscode/README.md](../editors/vscode/README.md) for the full extension settings list.
