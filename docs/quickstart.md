# Quickstart

This guide starts from a source checkout or a locally packaged VSIX. See [install.md](install.md)
for setup details.

## Open a Marrow project

1. Open a directory that contains `marrow.json` in VSCode.
2. Open a `.mw` file.
3. Confirm that the Marrow extension activates. Diagnostics should appear in the Problems panel
   when the canonical Marrow parser, checker, or schema logic reports an issue.

The editor surfaces Marrow facts; it does not redefine `.mw` language behavior. Use the Marrow
repository language docs as the semantic reference.

## Check editor features

- Run **Format Document** in a `.mw` file to use the LSP formatter.
- Use symbol, completion, hover, signature help, semantic token, go-to definition, find references,
  and supported rename workflows from normal VSCode commands.
- Keep `marrow.server.path` unset for packaged or development-host defaults, or set it to an
  absolute `marrow-lsp` path when testing a custom binary.

## Inspect saved resources

The Saved Resource Inspector is opt-in:

1. Enable `marrow.liveData`.
2. Open the Marrow Activity Bar view.
3. Refresh or reveal a server-returned saved root.

The view is read-only. It uses Marrow-owned store facts and boundary metadata, and it treats a
disabled, busy, or unavailable store as unavailable rather than as an editor-side data error.

## Launch the debugger

Press F5 in a Marrow project to use the `marrow-dap` debug adapter.

- Omit `entry` to use the project's `defaultEntry`.
- Set `entry` and typed `args` in `launch.json` when you need an explicit launch target.
- Use statement-level stepping, breakpoints, watch/REPL/hover evaluation, variables, and the
  Durable Data scope for supported standalone launches.

Served-process attach is not advertised yet. The adapter keeps that surface absent until Marrow
exposes the required served-process control boundary facts.
