# marrow-lsp agent instructions

This repository supplies editor, debugger, and automation tooling for Marrow. Its current products
are a VS Code extension, an LSP server, an optional MCP server, and a DAP debugger. These are tooling
clients of the Marrow language; they do not define it.

## Authority

Use the Marrow repository in this order:

1. `docs/language/` for current language behavior;
2. `docs/vision.md` for product direction and non-goals;
3. `docs/status.md` for the boundary between implemented, legacy, designed, and research work;
4. `docs/design/` for explicitly human-approved unimplemented target contracts;
5. `docs/implementation/` and the canonical Marrow crates for implementation facts.

If prose and code disagree about current behavior, establish the discrepancy with a minimal fixture
and repair the canonical Marrow source. Do not create a local interpretation in marrow-lsp. Old ADRs,
drafts, and orchestration reports are historical evidence, not authority.

## Current architecture

Today, language intelligence is concentrated in `marrow-lsp-core`. The `marrow-lsp` LSP transport,
`marrow-mcp` MCP transport, and `marrow-dap` debugger expose parts of it. Preserve that single-owner
shape while it is current; do not present the exact transport topology as a permanent language
contract.

Parsing, checking, typing, schemas, catalogs, store access, and execution come from canonical Marrow
crates. The tooling layer may select, cache, version, and present semantic facts. It must not reparse
or reconstruct:

- types or language constructs;
- stable schema identity or typed semantic paths;
- public URI/path projections;
- authority requirements or durable effects;
- evolution verdicts;
- runtime and storage meaning.

When a client needs a missing fact, add the narrow typed fact upstream in Marrow, then consume it
here. Facts should support selective retrieval and snapshot/version semantics so editors, CI,
automation, and agents observe one coherent program state. Rendered diagnostic text is output, not a
semantic API.

## Dependency boundary

Use path dependencies for local development and one pinned Marrow Git revision for releases in
`pins/marrow-rev.toml`. Bump the pin deliberately and review it with the regenerated
`pins/consumed-marrow-surface.snapshot`. That filename records a current implementation artifact; it
does not endorse “surface” as Marrow's long-term architecture.

Currently `marrow-lsp-core` is the main consumer of the upstream syntax, checking, catalog, JSON,
schema, project, store, and runtime crates. The LSP transport routes through the core. MCP also links
`marrow-run` for project-store-isolated in-memory execution. DAP currently links syntax, checking,
project, store, and runtime internals directly; this is a known current exception, not a pattern to
extend. The `dependency_shape` architecture test enforces the shipped dependency boundary. Test-only
dependencies do not redefine that production shape.

## LSP, MCP, and DAP

Keep transports thin. A transport owns protocol conversion, cancellation, lifecycle, and rendering;
it does not own language semantics. A capability that applies across clients belongs in the core or,
when semantic, upstream in Marrow.

MCP is one optional consumer of structured facts and execution controls. Do not make agents the
product audience or add speculative agent-only semantics. Prefer the same typed facts used by editors
and CI.

The debugger currently uses an opt-in runtime step hook so ordinary execution does not pay for debug
events. Read-only inspection currently opens the store read-only and treats a busy or unreadable store
as unavailable. Document these as present behavior and limitations; do not generalize them into
durability, concurrency, or security guarantees.

## Rust and TypeScript boundary

Rust owns analysis, semantic state, runtime integration, and transport servers. The TypeScript VS Code
extension bundles and launches binaries, contributes the grammar and UI, and adapts editor APIs. It
does not parse Marrow, calculate semantic positions independently, interpret projects, infer schemas,
or reproduce diagnostics.

Use typed IDs, versioned snapshots, typed facts, and structured errors. Avoid raw paths, rendered-text
matching, boolean mode flags, broad JSON bags, and duplicate protocol-independent classifiers. No
Rust `unsafe`.

## Documentation and claims

Write in a neutral reference style. Label behavior as current, legacy, designed, accepted target, or
research. Use “compiler-enforced,” “runtime-enforced,” “conformance-tested,” “measured,” or “formally
established” only when the repository contains that evidence. Do not describe future path
authorization, URI projection, native compilation, scale, safety, or multi-user behavior as
implemented.

Examples and protocol transcripts presented as complete must pass against the pinned Marrow revision.
When the pin changes, audit user-facing examples, diagnostics, debugger behavior, and semantic fact
adapters together.

## Working method

Follow the parent workspace instructions for worktrees, mandatory lane-local Cargo target directories,
review, and integration. Keep build output, packaged extensions, temporary fixtures, and probe logs
outside the tracked repository.

Start behavior changes with a failing production-path check and run the narrowest test that
establishes the invariant first. Every code integration then requires an explicit full Rust workspace
build, full Rust workspace tests, `fmt --check`, `clippy -D warnings`, and zero `unsafe`. Run
TypeScript typecheck/lint, extension tests, and packaged-extension smoke tests in proportion to the
affected TypeScript and cross-stack layers. A cross-stack change must prove that the extension
launches the bundled binary and completes a real request through every affected transport.

Review semantic changes upstream first, then review adapters for version, cancellation, stale-result,
and transport-boundary failures. Do not add a fallback parser, vendored Marrow type, compatibility
branch, or speculative transport feature to preserve an obsolete test.
