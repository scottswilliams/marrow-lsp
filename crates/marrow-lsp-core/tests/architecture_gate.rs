//! Whole-workspace no-local-semantics gate.
//!
//! `marrow-lsp` never re-decides `.mw` semantics: parsing, checking, typing, schema, catalog,
//! storage, and runtime facts come only from the canonical marrow crates, and each editor surface
//! renders those facts rather than re-deriving them locally. This one gate enforces that invariant
//! structurally, replacing the per-file `include_str!` substring scans that used to sit in each
//! surface's test module.
//!
//! Two failure modes those scattered scans could not catch, this gate does:
//!   * **Relocation.** A hand-picked single-file scan is defeated by moving the classifier one file
//!     over. Each surface denial here covers the surface's whole module (its `<name>.rs` entry plus
//!     everything under `<name>/`), and the [`WORKSPACE_DENIED`] definitions are refused in *every*
//!     production file across *every* crate — a relocated classifier or a brand-new mapper module
//!     cannot bypass it.
//!   * **Silent removal.** Deleting a scan silently drops its invariant. Gathering them here keeps
//!     the full set visible and gated in one place.
//!
//! Scope discipline. A denied fragment is refused only where it would be a local semantic decision;
//! the sanctioned home of a genuinely shared spelling is left untouched (e.g. `TokenKind` is a local
//! reclassification inside `hover`/`completion` but the legitimate vocabulary of the presentation
//! `indentation` re-lexer and the token encoder). So most denials are surface-scoped, and only
//! definition-site fragments (`fn foo`, `struct Foo`, `mod foo`) — which can never be a false
//! positive, since defining the named local classifier anywhere is the violation — are workspace-wide.
//!
//! Test code is exempt: a `#[cfg(test)] mod tests { ... }` block and the `tests.rs` submodule files
//! legitimately lex tokens, build stores, and list these very fragments as string literals, so they
//! are stripped before scanning (see the `support` module).

mod support;

use std::path::{Path, PathBuf};
use support::{production_rust_sources, production_source};

/// Where a surface denial applies, relative to a crate's `src` directory.
#[derive(Clone, Copy, Debug)]
enum Scope {
    /// The module entry file `<name>.rs` plus every file under `<name>/`.
    Module(&'static str),
    /// Exactly one file, by its path relative to `src`.
    File(&'static str),
}

impl Scope {
    fn covers(self, rel: &str) -> bool {
        match self {
            Scope::Module(name) => {
                rel == format!("{name}.rs") || rel.starts_with(&format!("{name}/"))
            }
            Scope::File(path) => rel == path,
        }
    }
}

struct Surface {
    krate: &'static str,
    scope: Scope,
    /// Why this surface must not decide these semantics locally — rendered on failure.
    rationale: &'static str,
    denied: &'static [&'static str],
}

/// Definition-site fragments refused in every production file across the workspace. Defining any of
/// these local classifiers *anywhere* is the violation, so a workspace-wide scan cannot false-match:
/// there is no legitimate home for a local outline/doc/schema/callable/saved-place reclassifier.
const WORKSPACE_DENIED: &[&str] = &[
    // Local classifier modules a surface might try to reintroduce under a new name.
    "mod active_call",
    "mod saved_roots",
    "mod source_names",
    "mod type_context",
    // Local hover symbol/callable/schema traversal.
    "fn is_function_hover_target",
    "fn is_rich_symbol_hover_target",
    "fn rich_symbol_fact",
    "fn blocked_enum_type_symbol_hover",
    "fn resource_schema_for_symbol",
    "fn enum_schema_for_symbol",
    "fn enum_member_schema_for_symbol",
    "fn enum_annotation_fact",
    "fn declaration_docs",
    "fn enum_member_docs",
    "fn member_docs",
    "fn parsed_function_at",
    "fn parsed_const_at",
    "fn function_fact",
    "fn parameter_fact",
    "fn parameter_use_name",
    "fn module_const_fact",
    "fn checked_function_for_parsed",
    "fn checked_function_for_fact",
    "fn checked_const_at",
    "fn function_fact_for_symbol",
    "fn store_root_at",
    "fn saved_root_span",
    "fn default_library_call_text",
    "fn operator_text",
    "fn operator_spelling",
    // Local document/workspace outline assembly.
    "fn declaration_symbol",
    "fn evolve_symbol",
    "fn enum_symbol",
    "fn resource_symbol",
    "fn store_symbol",
    "fn surface_symbol",
    "fn declaration_owner",
    "fn catalog_symbol_kind",
    "fn source_symbol_match",
    // Local completion candidate assembly.
    "fn root_completions",
    "fn saved_path_completions",
    "fn namespace_completions",
    "fn type_completions",
    "fn bare_completions",
    // Local MCP run-DTO renderers and value budget.
    "struct ValueBudget",
    "struct CatalogContainers",
];

/// Surface-scoped denials. Each fragment names a local semantic decision that must instead consume a
/// Marrow fact; the scope is the surface where that decision would live, extended from the original
/// single scanned file to the whole module so a relocation within the surface is still caught.
const SURFACES: &[Surface] = &[
    Surface {
        krate: "marrow-lsp-core",
        scope: Scope::File("store.rs"),
        rationale: "SavedDataSession must delegate admission and watch targets to marrow-run/data-view facts, not stitch digests or derive store paths locally",
        denied: &[
            "read_only_context_matches",
            "source_digest() ==",
            "read_only_context_digest() ==",
            ".catalog.accepted_digest",
            "CATALOG_FILE_NAME",
            "native_store_path",
            "StoreBackend",
        ],
    },
    Surface {
        krate: "marrow-lsp-core",
        scope: Scope::Module("signature_help"),
        rationale: "signature help must consume marrow_check intrinsic/active-call/callable facts, not model callables or active calls locally",
        denied: &[
            "signature_from_label",
            "bare_function_builtins",
            "scalar_conversion_detail",
            "marrow_schema::stdlib",
            "std_param_label",
            "std_return_label",
            "active_call::active_call",
            "resolve(",
            "DefItem",
            "ResolvableKind",
            "resource_constructor_signature",
            "intrinsic_callable_signature_for_file",
            "FunctionDecl",
            ".declarations",
        ],
    },
    Surface {
        krate: "marrow-lsp-core",
        scope: Scope::Module("hover"),
        rationale: "hover must render Marrow source hover facts, not own intrinsic/operator/doc/callable/saved-place/schema/module-path traversal",
        denied: &[
            "language_facts::bare_builtin_hover",
            "language_facts::scalar_conversion_hover",
            "language_facts::std_operation_hover",
            "intrinsic_callable_signature_for_file",
            "lex_source",
            "TokenKind",
            "language_facts",
            "super::tokens",
            "type_at",
            "checked_type_at",
            "is_resource_member_hover_target",
            "is_saved_member_declaration_name",
            "is_resource_member_declaration_name",
            "is_index_declaration_name",
            "resource_member_at",
            "store_index_at",
            "mod source",
            "enum_schema_in_file",
            "enum_member_path_at",
            "is_named_symbol_reference",
            "is_symbol_declaration_name",
            "std_library_path_text",
            "blocked_module_prefix_hover",
            "std_operation_call_for_segment",
            "module_path_at_with_position",
            "stdlib::all()",
        ],
    },
    Surface {
        krate: "marrow-lsp-core",
        scope: Scope::File("mcp.rs"),
        rationale: "mw_run must go through ProjectOpen::test() and consume Marrow-owned run DTOs, not local renderers or entry-call assembly",
        denied: &[
            "check_tests_program",
            "CheckedEntryCall::new",
            "run_entry_with_host",
            "fn run_facts_json",
            "fn run_output_json",
            "fn runtime_error_json",
            "fn project_invoke_error_json",
            "fn project_session_error_json",
            "fn value_to_json",
            "entry_descriptor_error_message",
            "mcp.run.entry",
            "RUN_VALUE_NODE_CAP",
            "RUN_VALUE_STRING_CAP",
        ],
    },
    Surface {
        krate: "marrow-lsp-core",
        scope: Scope::File("completion.rs"),
        rationale: "completion must request Marrow's aggregate completion fact, not own saved-root/type/std/cursor-recovery models or assemble candidates locally",
        denied: &[
            "saved_root_stores",
            "module.stores",
            "bare_function_builtins",
            "scalar_conversion_names",
            "scalar_conversion_detail",
            "TYPE_NAMES",
            "fn resources(",
            "fn stores(",
            "fn enums(",
            "marrow_schema::stdlib",
            "std_root_module_completions",
            "std_module_completions",
            "std_module_exists",
            "ParamType",
            "ReturnType",
            "fn std_signature(",
            "TokenKind",
            "marrow_syntax::Token",
            "lexed.tokens",
            ".tokens.iter()",
            "significant_tokens",
            "matching_open_",
            "saved_receiver",
            "qualifier_before",
            "introduces_type",
            "const KEYWORDS",
            "SourceSavedRootCompletionCandidate",
            "SourceTypeCompletionCandidate",
            "SourceNamespaceCompletionFact",
            "DeclaredDataChild",
            "source_saved_root_completion_fact",
            "source_type_completion_fact",
            "source_namespace_completion_fact",
            "declared_source_receiver_data_children",
            "intrinsic_completion_callables",
            "scope_at",
        ],
    },
    Surface {
        krate: "marrow-lsp-core",
        scope: Scope::Module("navigation"),
        rationale: "navigation must consume Marrow cursor/catalog/rename facts, not inspect tokens or classify saved-root/type-annotation/identifier syntax locally",
        denied: &[
            "root_syntax_at",
            "lex_source",
            "TokenKind",
            "crate::type_context",
            "source_type_annotation_cursor_fact_at",
            "CatalogEntryKind",
            "UseSiteKind",
            ".use_sites()",
            ".catalog_declarations()",
            "supported_use_site",
            "supported_declaration",
            "is_valid_rename",
            "name_in_span_at",
        ],
    },
    Surface {
        krate: "marrow-lsp-core",
        scope: Scope::File("symbols.rs"),
        rationale: "document/workspace symbols must consume marrow-check outline and source-symbol facts, not own outline assembly or catalog classification",
        denied: &[
            "fn function_signature",
            "fn name_span",
            "Declaration::",
            "ResourceMember::",
            "CatalogEntryKind",
            "sort_by_key",
        ],
    },
    Surface {
        krate: "marrow-lsp-core",
        scope: Scope::File("semantic_tokens.rs"),
        rationale: "semantic tokens must map Marrow role facts, not own token/style/symbol role classification",
        denied: &[
            "TokenKind",
            "TokenStyle",
            "SymbolKind",
            "declaration_overrides",
            "builtin_overrides",
            "reference_overrides",
            "type_annotation_overrides",
            "token_type(",
        ],
    },
    Surface {
        krate: "marrow-mcp",
        scope: Scope::File("server.rs"),
        rationale: "MCP must consume the Marrow saved-data DTO schema helpers, not keep local saved-data schema authorities",
        denied: &["fn data_key_schema", "fn data_path_segment_schema"],
    },
];

/// A fact-consumption spelling every surface must keep: deleting the consuming call while leaving the
/// surface otherwise clean would silently re-open the door to a local model, so presence is required.
struct Required {
    krate: &'static str,
    rel: &'static str,
    /// Each `(needle, min_count)` must appear at least `min_count` times in the file's production source.
    needles: &'static [(&'static str, usize)],
}

const REQUIRED: &[Required] = &[
    Required {
        krate: "marrow-lsp-core",
        rel: "hover/facts.rs",
        needles: &[("source_type_hover_fact_at", 1)],
    },
    Required {
        krate: "marrow-lsp-core",
        rel: "completion.rs",
        needles: &[("source_completion_fact", 1)],
    },
    Required {
        krate: "marrow-lsp-core",
        rel: "navigation/symbols.rs",
        needles: &[
            ("source_saved_root_cursor_fact_at", 1),
            ("source_only_rename_occurrence_at", 1),
            ("source_only_rename_action_at", 1),
        ],
    },
    Required {
        krate: "marrow-lsp-core",
        rel: "navigation/catalog_uses.rs",
        needles: &[
            ("source_catalog_definition_fact_at", 1),
            ("source_catalog_reference_facts_at", 1),
        ],
    },
    Required {
        krate: "marrow-mcp",
        rel: "server.rs",
        needles: &[
            ("data_key_input_schema(", 1),
            ("data_path_segment_input_schema()", 2),
        ],
    },
];

/// Classifier modules a surface must not keep as its own file. The absence of the file is the
/// enforcement; a resurrected `type_context`/`active_call`/per-role token module would fail here.
const ABSENT_FILES: &[(&str, &str)] = &[
    ("marrow-lsp-core", "signature_help/active_call.rs"),
    ("marrow-lsp-core", "signature_help/signatures.rs"),
    ("marrow-lsp-core", "semantic_tokens/builtins.rs"),
    ("marrow-lsp-core", "semantic_tokens/declarations.rs"),
    ("marrow-lsp-core", "semantic_tokens/references.rs"),
    ("marrow-lsp-core", "semantic_tokens/syntax.rs"),
    ("marrow-lsp-core", "semantic_tokens/type_annotations.rs"),
];

/// Dispatch order the public navigation path must keep: Marrow catalog navigation is tried before the
/// BindingIndex fallback. Encoded as `before` appearing ahead of `after` in the file's production source.
const ORDER: &[(&str, &str, &str, &str)] = &[
    (
        "marrow-lsp-core",
        "navigation/symbols.rs",
        "catalog_uses::definition",
        "index.definition(file, offset)",
    ),
    (
        "marrow-lsp-core",
        "navigation/symbols.rs",
        "catalog_uses::references",
        "let definition = index.definition(file, offset)?",
    ),
];

/// The workspace crate roots this gate walks. `marrow-lsp-core` is the language-intelligence consumer;
/// the transports are included so a local classifier cannot hide in a transport binary.
const CRATES: &[&str] = &[
    "marrow-lsp-core",
    "marrow-lsp",
    "marrow-mcp",
    "marrow-dap",
    "marrow-terminal",
];

/// One production source file: its crate, its path relative to that crate's `src`, and the
/// test-stripped source text scanned for denied fragments.
struct ProdFile {
    krate: &'static str,
    rel: String,
    production: String,
}

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir")
        .to_path_buf()
}

fn load_production_files() -> Vec<ProdFile> {
    let crates = crates_dir();
    let mut files = Vec::new();
    for &krate in CRATES {
        let src = crates.join(krate).join("src");
        let mut paths = Vec::new();
        production_rust_sources(&src, &mut paths);
        for path in paths {
            let rel = path
                .strip_prefix(&src)
                .expect("path under src")
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&path).expect("read source");
            files.push(ProdFile {
                krate,
                rel,
                production: production_source(&text),
            });
        }
    }
    assert!(
        files.len() > 20,
        "expected the whole workspace to be walked, found only {} files",
        files.len()
    );
    files
}

fn file_production<'a>(files: &'a [ProdFile], krate: &str, rel: &str) -> &'a str {
    files
        .iter()
        .find(|f| f.krate == krate && f.rel == rel)
        .map(|f| f.production.as_str())
        .unwrap_or_else(|| panic!("expected production file {krate}/src/{rel} to exist"))
}

#[test]
fn no_surface_reimplements_marrow_semantics_locally() {
    let files = load_production_files();
    let mut violations: Vec<String> = Vec::new();

    // Workspace-wide definition-site denials: refused in every production file.
    for file in &files {
        for &needle in WORKSPACE_DENIED {
            if file.production.contains(needle) {
                violations.push(format!(
                    "{}/src/{} defines `{needle}`: the LSP must not own this classifier anywhere; \
                     consume the Marrow fact instead",
                    file.krate, file.rel
                ));
            }
        }
    }

    // Surface-scoped denials: refused across each surface's whole module.
    for surface in SURFACES {
        let mut matched = 0usize;
        for file in &files {
            if file.krate != surface.krate || !surface.scope.covers(&file.rel) {
                continue;
            }
            matched += 1;
            for &needle in surface.denied {
                if file.production.contains(needle) {
                    violations.push(format!(
                        "{}/src/{} contains `{needle}`: {}",
                        file.krate, file.rel, surface.rationale
                    ));
                }
            }
        }
        // A surface whose scope matches no file would pass vacuously — the compile-time
        // existence the old `include_str!` scans gave. A rename that orphans the scope must
        // fail here, not silently drop the surface's denials.
        assert!(
            matched > 0,
            "surface scope {:?} in {} matched no production file — the scope is stale (a rename?), \
             so its denials pass vacuously",
            surface.scope,
            surface.krate
        );
    }

    assert!(
        violations.is_empty(),
        "marrow-lsp production code re-decides marrow semantics locally; route each through the \
         canonical Marrow fact instead:\n{}",
        violations.join("\n")
    );
}

#[test]
fn surfaces_keep_consuming_the_marrow_facts() {
    let files = load_production_files();
    let mut missing = Vec::new();
    for req in REQUIRED {
        let source = file_production(&files, req.krate, req.rel);
        for &(needle, min) in req.needles {
            let count = source.matches(needle).count();
            if count < min {
                missing.push(format!(
                    "{}/src/{} calls `{needle}` {count} time(s), expected at least {min}: the \
                     surface must consume this Marrow fact",
                    req.krate, req.rel
                ));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "a surface dropped its Marrow fact consumption:\n{}",
        missing.join("\n")
    );
}

#[test]
fn no_local_classifier_modules_survive_as_files() {
    let crates = crates_dir();
    let mut present = Vec::new();
    for &(krate, rel) in ABSENT_FILES {
        let path = crates.join(krate).join("src").join(rel);
        if path.exists() {
            present.push(format!("{krate}/src/{rel}"));
        }
    }
    assert!(
        present.is_empty(),
        "a local classifier module was resurrected as a file; its logic belongs in marrow-check \
         tooling:\n{}",
        present.join("\n")
    );
}

#[test]
fn navigation_prefers_marrow_facts_before_the_binding_fallback() {
    let files = load_production_files();
    let mut out_of_order = Vec::new();
    for &(krate, rel, before, after) in ORDER {
        let source = file_production(&files, krate, rel);
        let before_at = source.find(before);
        let after_at = source.find(after);
        match (before_at, after_at) {
            (Some(b), Some(a)) if b < a => {}
            _ => out_of_order.push(format!(
                "{krate}/src/{rel}: `{before}` must appear before `{after}`"
            )),
        }
    }
    assert!(
        out_of_order.is_empty(),
        "navigation dispatch order regressed (Marrow facts must be tried before BindingIndex):\n{}",
        out_of_order.join("\n")
    );
}

// --- Self-tests: the gate's scanning primitives have teeth. ---

#[test]
fn scope_matching_is_module_and_file_precise() {
    let hover = Scope::Module("hover");
    assert!(hover.covers("hover.rs"));
    assert!(hover.covers("hover/render.rs"));
    assert!(!hover.covers("hoverer.rs"));
    assert!(!hover.covers("semantic_tokens.rs"));

    let entry = Scope::File("semantic_tokens.rs");
    assert!(entry.covers("semantic_tokens.rs"));
    // Entry-only: a sibling under the module directory is out of scope (the token encoder
    // legitimately handles tokens), so the file scope must not leak into `semantic_tokens/`.
    assert!(!entry.covers("semantic_tokens/encoding.rs"));
}

#[test]
fn a_planted_local_classifier_is_detected() {
    // A denied definition planted in otherwise-clean production text is caught by the same
    // membership test the gate runs, and stripped when it lives in a test module.
    let relocated = "fn is_function_hover_target(offset: usize) -> bool { false }\n";
    assert!(WORKSPACE_DENIED.iter().any(|n| relocated.contains(n)));

    let in_tests = format!("#[cfg(test)]\nmod tests {{\n    {relocated}}}\n");
    assert!(
        !production_source(&in_tests).contains("fn is_function_hover_target"),
        "a fragment inside a test module must be stripped before scanning"
    );
}
