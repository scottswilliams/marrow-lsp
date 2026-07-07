//! Architecture gate for the one-core dependency contract (roadmap S0.4).
//!
//! `marrow-lsp-core` is the sole language-intelligence consumer: it links the full set of
//! upstream marrow crates. The transport binaries are allowed a *bounded* direct link to a
//! small set of marrow crates for transport-level concerns (running a program, loading a
//! project, opening a store view, rendering) — but that set is recorded here, so a new
//! accidental `transport -> marrow` link, or a transport reaching for a language-semantics
//! crate it should route through the core, fails this gate loudly.
//!
//! This is the enforcement artifact for the AGENTS/ledger record of the bounded DAP/MCP/LSP
//! exception to "the core is the only crate that links marrow". Changing a crate's marrow
//! dependency set is a deliberate edit that updates the expected map below in the same change.

use std::collections::BTreeSet;
use std::path::Path;

/// The upstream marrow crates (everything under `../marrow`). `marrow-terminal` is a local
/// crate in this workspace, not an upstream link, so it is not tracked here.
const UPSTREAM: &[&str] = &[
    "syntax", "check", "catalog", "json", "schema", "project", "store", "run", "codes",
];

/// The recorded *production* direct-link contract: crate -> the upstream marrow crates it
/// names in `[dependencies]`. Test-only links (`[dev-dependencies]`) are not part of the
/// shipped one-core shape and are not tracked here.
fn expected(crate_name: &str) -> Option<BTreeSet<&'static str>> {
    let set: &[&str] = match crate_name {
        // The sole language-intelligence consumer links the full set.
        "marrow-lsp-core" => &[
            "syntax", "check", "catalog", "json", "schema", "project", "store", "run",
        ],
        // The LSP transport routes everything through the core: no production upstream link.
        "marrow-lsp" => &[],
        // The MCP transport links only run, for sandboxed program execution.
        "marrow-mcp" => &["run"],
        // The recorded broad exception: the debugger drives runtime/debug/store internals
        // directly. Narrowing this is roadmap Lane S0.5's follow-on (route through the core).
        "marrow-dap" => &["syntax", "check", "project", "store", "run"],
        "marrow-terminal" => &[],
        _ => return None,
    };
    Some(set.iter().copied().collect())
}

/// The upstream marrow crates named under `[dependencies]` only — the production link set.
/// Section tracking stops the scan at `[dev-dependencies]`/`[build-dependencies]`, so a
/// test-support link is not counted as a shipped dependency.
fn linked_upstream(manifest: &Path) -> BTreeSet<&'static str> {
    let text = std::fs::read_to_string(manifest).expect("read Cargo.toml");
    let mut found = BTreeSet::new();
    let mut in_dependencies = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_dependencies = trimmed == "[dependencies]";
            continue;
        }
        if !in_dependencies {
            continue;
        }
        for name in UPSTREAM {
            let prefix = format!("marrow-{name}");
            // A dependency line is `marrow-<name>.workspace = ...` or `marrow-<name> = { ... }`.
            if let Some(rest) = trimmed.strip_prefix(&prefix)
                && (rest.starts_with('.') || rest.trim_start().starts_with('='))
            {
                found.insert(*name);
            }
        }
    }
    found
}

#[test]
fn transport_marrow_links_match_the_recorded_contract() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir");
    let crates = [
        "marrow-lsp-core",
        "marrow-lsp",
        "marrow-mcp",
        "marrow-dap",
        "marrow-terminal",
    ];
    let mut violations = Vec::new();
    for name in crates {
        let manifest = workspace.join(name).join("Cargo.toml");
        let actual = linked_upstream(&manifest);
        let expected = expected(name).expect("every workspace crate has a recorded contract");
        if actual != expected {
            violations.push(format!(
                "{name}: links {actual:?} but the recorded contract is {expected:?}"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "a crate's upstream-marrow link set drifted from the one-core contract; if intended, \
         update the expected map in this test in the same change:\n{}",
        violations.join("\n")
    );
}
