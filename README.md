# marrow-lsp

Editor and agent toolchain for the [Marrow](https://github.com/scottswilliams/marrow) `.mw`
language: a VSCode extension, a Language Server (LSP), an MCP server for coding agents, and a
DAP debugger. They are three transports over one Rust brain.

All language intelligence lives in `marrow-lsp-core`, which reuses the canonical marrow crates
(`marrow-syntax`, `marrow-check`, `marrow-catalog`, `marrow-json`, `marrow-schema`,
`marrow-store`, `marrow-run`, `marrow-project`) and never reimplements the parser, checker, or
type system. The marrow repo's `docs/language/` is the source of truth for `.mw` behavior.

## Layout

```
crates/
  marrow-lsp-core/   the transport-free language intelligence
  marrow-lsp/        LSP server over stdio (thin transport)
  marrow-mcp/        MCP server over stdio (thin transport)
  marrow-dap/        DAP debug adapter (thin transport)
  marrow-terminal/   terminal styling helpers shared by the transport binaries
editors/vscode/      VSCode extension: launcher + grammar + views + debug wiring
fixtures/shelf/      a runnable Marrow project used by tests and manual checks
xtask/               workspace maintenance (consumed-marrow-surface drift snapshot)
```

## Status

The workspace contains the shared Rust core, the LSP/MCP/DAP transports, and the VSCode
extension. Diagnostics, completion, hover, navigation, semantic tokens, formatting, on-type
indentation, read-only data inspection, MCP tools, and statement-level debugging are
implemented, with language behavior coming from the canonical marrow crates.

Some surfaces are intentionally absent until Marrow exposes the production facts they need:
data integrity and repair, live saved-data hover and record counts, saved-data-backed editor
rename, and attaching to served (running) programs. The
[production readiness roadmap](docs/roadmaps/production-readiness-roadmap.md) is the plan from
the current state to production; the [fact-consumption ledger](docs/roadmaps/lsp-fact-consumption-ledger.md)
records what each surface may treat as semantic authority. See `AGENTS.md` for working
conventions.

## MCP trust boundary

The MCP server (`marrow-mcp`) is a data-exfiltration and mutation surface when data access is
enabled, so its reach is bounded and stated up front. A data-enabled server can reach exactly
this much:

- **Confined to one root.** Data, source, and run tools operate only on projects whose resolved
  root is under the operator's allowed root (`--root <dir>` or `MARROW_MCP_ROOT`, defaulting to
  the launch working directory). A file whose canonical location, or whose project's `marrow.json`,
  resolves outside the root is refused with `path.out_of_root` before any store is opened.
- **`mw_run` is sandboxed.** It executes project source but only against a fresh in-memory store;
  the project's real store is never opened.
- **The write grant is coarse.** With `--allow-write`, the grant is as strong as the project's most
  destructive surface action; it is not scoped to individual records.
- **The audit trail is operational, not tamper-evident.** Every granted write appends a record
  (timestamp, operation, target identity, outcome, store identity before/after) to
  `marrow-mcp-write-audit.jsonl` beside the project — a record of what an agent changed, not a
  secured log.
- **Grants come only from launch configuration.** Read access, write access, and the allowed root
  are set by launch flags or environment variables and are fixed for the session; no tool call can
  widen them.
## Releasing

A version tag (`v*`) drives `.github/workflows/release.yml`: it builds one `.vsix` per
platform against the pinned marrow revision, attests build provenance, and publishes to the
marketplace behind the protected `marketplace` environment. The items below are owner actions
and known residuals that the pipeline does **not** yet enforce; do them by hand before and
during a release.

### Before the first marketplace publish

- Create and **verify** the `marrow-lang` marketplace publisher (until then the publisher id
  in `package.json` is a placeholder that is not a registered or verified publisher).
- Scope the `VSCE_PAT` to that publisher only, and store it as the `VSCE_PAT` secret on the
  `marketplace` environment.
- Require reviewers on the `marketplace` environment so a publish is a deliberate, approved
  action rather than an automatic side effect of a tag push.
- Protect `v*` tags (a tag protection rule) so only maintainers can cut a release tag.

### Known residuals

- **Tag is not asserted CI-green.** The release builds and version-stamps the tagged commit,
  but it does not verify that the tagged sha passed CI first. Confirm the tag's CI run is green
  before pushing the tag or dispatching the release.
- **linux-arm64 is cross-linked and launch-unverified.** Four platforms
  (`darwin-arm64`, `darwin-x64`, `linux-x64`, `win32-x64`) build on a native runner and are
  launched through a real LSP `initialize`/`exit` handshake in their build job
  (`scripts/lspHandshakeSmoke.mjs`). `linux-arm64` is cross-compiled on an x64 runner, so its
  binary cannot run there and its handshake smoke is skipped; it stays a launch-unverified leg
  until an arm64 Linux runner is available.
- **Partial-publish runbook.** Marketplace versions are immutable. If a publish fails after
  some platforms have gone out, do **not** leave a partial listing: re-run the publish (each
  `vsce publish --packagePath` is idempotent for a target that already exists, so re-publishing
  the remaining targets is safe), or if a target cannot be re-published under the same version,
  bump the patch version and publish a complete set. Never ship a listing missing platforms.

### Untrusted-workspace activation posture

The extension keeps `capabilities.untrustedWorkspaces.supported: "limited"`: in VS Code
Restricted Mode the LSP still provides diagnostics and hover, because activation only parses and
type-checks — it never evaluates workspace code. This was verified in the marrow-lsp backend:

- The LSP request handlers in `crates/marrow-lsp/src/backend.rs` never call the runtime's
  execution entry points. Program execution (`run_entry`, `RunMode::Run`) lives only in
  `crates/marrow-lsp-core/src/mcp.rs`, driven by the MCP server that coding agents launch — not
  by the editor on folder-open. `marrow-dap` (a debugger the user starts with F5, under trust)
  is the other execution path; neither runs on LSP activation.
- Store access is read-only and opt-in: `marrow.liveData` defaults to `false` and is now
  restricted under untrusted workspaces, and even when enabled the store is opened through the
  read-only `read_accepted_catalog_with_store_read_only` path, with a busy or unreadable store
  treated as a soft "unavailable" (`crates/marrow-lsp-core/src/store_view.rs`).
- The whole Rust workspace sets `unsafe_code = "forbid"` (`Cargo.toml`), so pre-trust activation
  only reads bytes with no `unsafe` in the tree.

So a folder opened without trust exercises only parse/check and, at most, a read-only store read
gated behind an off-by-default preference that an untrusted workspace can no longer flip.
