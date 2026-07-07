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

/// Whether a `[...]` header opens a *production* dependency section, and — for the explicit
/// table form `[dependencies.marrow-store]` — the crate named in the header itself. Handles the
/// list form (`[dependencies]`), the table form, and the platform-gated forms
/// (`[target.'cfg(...)'.dependencies]` and its table variant). Test- and build-only sections
/// return `not a prod section`, so a `[dev-dependencies]` link is never counted as shipped.
fn prod_dependency_section(header: &str) -> Option<Option<&str>> {
    // Strip the brackets and any trailing inline comment before matching.
    let inner = header
        .trim()
        .strip_prefix('[')?
        .split('#')
        .next()
        .unwrap_or("")
        .trim()
        .strip_suffix(']')?;
    if inner.contains("dev-dependencies") || inner.contains("build-dependencies") {
        return None;
    }
    // Locate the `dependencies` segment; it must sit at the start or after a `.` boundary.
    let pos = inner.rfind("dependencies")?;
    if pos != 0 && !inner[..pos].ends_with('.') {
        return None;
    }
    match inner[pos + "dependencies".len()..].strip_prefix('.') {
        None if inner[pos + "dependencies".len()..].is_empty() => Some(None),
        Some(table_crate) => Some(Some(table_crate)),
        None => None,
    }
}

/// The upstream marrow crates named under any production `[dependencies]` section — the shipped
/// link set. Test-support links (`[dev-dependencies]`) are excluded, and both the list form and
/// the `[dependencies.<crate>]` / `[target.*.dependencies]` table forms are covered so a link
/// hidden in a table or platform-gated section cannot evade the one-core gate.
fn linked_upstream(manifest: &Path) -> BTreeSet<&'static str> {
    let text = std::fs::read_to_string(manifest).expect("read Cargo.toml");
    parse_links(&text)
}

fn parse_links(text: &str) -> BTreeSet<&'static str> {
    let mut found = BTreeSet::new();
    let mut in_dependencies = false;

    let record = |found: &mut BTreeSet<&'static str>, candidate: &str| {
        if let Some(name) = candidate.strip_prefix("marrow-")
            && let Some(known) = UPSTREAM.iter().find(|n| **n == name)
        {
            found.insert(*known);
        }
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            match prod_dependency_section(trimmed) {
                Some(table_crate) => {
                    in_dependencies = true;
                    // `[dependencies.marrow-store]` names its crate in the header itself.
                    if let Some(name) = table_crate {
                        record(&mut found, name);
                    }
                }
                None => in_dependencies = false,
            }
            continue;
        }
        if !in_dependencies {
            continue;
        }
        // A list-form line is `marrow-<name>.workspace = ...` or `marrow-<name> = { ... }`.
        for name in UPSTREAM {
            let prefix = format!("marrow-{name}");
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

#[test]
fn parser_catches_table_and_target_form_dependencies() {
    // Ordinary list form under [dev-dependencies] must NOT count (test-only link).
    assert!(parse_links("[dev-dependencies]\nmarrow-check.workspace = true\n").is_empty());
    // The explicit-table form names the crate in the header; it must count.
    assert_eq!(
        parse_links("[dependencies.marrow-store]\nworkspace = true\n"),
        BTreeSet::from(["store"])
    );
    // A platform-gated dependencies section must count.
    assert_eq!(
        parse_links("[target.'cfg(unix)'.dependencies]\nmarrow-run.workspace = true\n"),
        BTreeSet::from(["run"])
    );
    // A platform-gated table form must count.
    assert_eq!(
        parse_links("[target.'cfg(unix)'.dependencies.marrow-project]\nworkspace = true\n"),
        BTreeSet::from(["project"])
    );
    // A platform-gated dev section must NOT count.
    assert!(
        parse_links("[target.'cfg(unix)'.dev-dependencies]\nmarrow-store.workspace = true\n")
            .is_empty()
    );
}
