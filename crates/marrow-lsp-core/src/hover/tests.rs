use super::*;
use lsp_types::HoverContents;
use marrow_check::tooling::{
    CallableSignature, CallableSignatureKind, SavedPlaceHoverFact, SourceCallableHoverFact,
    SourceEnumMemberStatus, SourceHoverFact, SourceModulePathHoverFact, SourceOperatorHoverFact,
    SourceResourceHoverFact, SourceResourceHoverMemberKind, SourceSchemaHoverFact,
    SourceTypeHoverFact, StoreRootHoverFact, source_hover_fact_at,
};
use marrow_check::{CheckedProgram, ProjectSources, analyze_project};
use marrow_project::parse_config;

/// Analyze a one-file project laid out on disk under a temp dir, returning the
/// snapshot and the module file's path so a test can hover into it.
fn analyze(source: &str) -> (AnalysisSnapshot, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("a.mw");
    std::fs::write(&file, source).unwrap();
    let config =
        parse_config(r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#).unwrap();
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None, None).unwrap();
    // Keep the temp dir alive for the snapshot's lifetime by leaking it; the
    // OS reclaims it when the test process exits.
    std::mem::forget(dir);
    (snapshot, file)
}

fn analyze_files(files: &[(&str, &str)], target: &str) -> (AnalysisSnapshot, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    for (path, source) in files {
        let file = src.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, source).unwrap();
    }
    let config =
        parse_config(r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#).unwrap();
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None, None).unwrap();
    let file = src.join(target);
    std::mem::forget(dir);
    (snapshot, file)
}

/// The byte offset of the first occurrence of `needle` in `source`.
fn offset_of(source: &str, needle: &str) -> usize {
    source.find(needle).expect("needle present in source")
}

fn hover_value(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &std::path::Path,
    source: &str,
    needle: &str,
) -> String {
    let offset = offset_of(source, needle);
    let hover = hover_with_index(snapshot, index, file, offset).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    markup.value
}

fn hover_value_at(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &std::path::Path,
    offset: usize,
) -> Option<String> {
    let hover = hover_with_index(snapshot, index, file, offset)?;
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    Some(markup.value)
}

/// The typed hover fact selected at the first byte of `needle` in a single-file project — the
/// oracle the LSP renders. Asserting the fact's variant and typed payload keeps a test anchored to
/// the semantic decision (which callable/kind/module was resolved) rather than a fragment of the
/// rendered Markdown, which rewords freely upstream.
fn analyzed_with_index(source: &str) -> (AnalysisSnapshot, BindingIndex, std::path::PathBuf) {
    let (snapshot, file) = analyze(source);
    let index = build_binding_index(&snapshot);
    (snapshot, index, file)
}

fn callable_fact_from(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &std::path::Path,
    offset: usize,
) -> SourceCallableHoverFact {
    match source_hover_fact_at(snapshot, index, file, offset) {
        Some(SourceHoverFact::Callable(fact)) => fact,
        _ => panic!("expected a source-callable hover fact at offset {offset}"),
    }
}

fn callable_fact(source: &str, needle: &str) -> SourceCallableHoverFact {
    let (snapshot, index, file) = analyzed_with_index(source);
    callable_fact_from(&snapshot, &index, &file, offset_of(source, needle))
}

/// The module-path hover fact at `offset`, or `None` when the resolved hover is not a module path.
/// The rendered "default library namespace/module." context is a pure render of the variant, and
/// the operation summary a render of the typed `operations` list.
fn module_path_fact_at(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &std::path::Path,
    offset: usize,
) -> Option<SourceModulePathHoverFact> {
    match source_hover_fact_at(snapshot, index, file, offset) {
        Some(SourceHoverFact::ModulePath(fact)) => Some(fact),
        _ => None,
    }
}

fn schema_fact_from(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &std::path::Path,
    offset: usize,
) -> SourceSchemaHoverFact {
    match source_hover_fact_at(snapshot, index, file, offset) {
        Some(SourceHoverFact::Schema(fact)) => fact,
        _ => panic!("expected a schema hover fact at offset {offset}"),
    }
}

fn saved_place_fact_from(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &std::path::Path,
    offset: usize,
) -> SavedPlaceHoverFact {
    match source_hover_fact_at(snapshot, index, file, offset) {
        Some(SourceHoverFact::SavedPlace(fact)) => fact,
        _ => panic!("expected a saved-place hover fact at offset {offset}"),
    }
}

fn saved_place_docs(fact: &SavedPlaceHoverFact) -> &[String] {
    match fact {
        SavedPlaceHoverFact::Field { docs, .. }
        | SavedPlaceHoverFact::Layer { docs, .. }
        | SavedPlaceHoverFact::Index { docs, .. } => docs,
    }
}

fn type_fact_from(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &std::path::Path,
    offset: usize,
) -> SourceTypeHoverFact {
    match source_hover_fact_at(snapshot, index, file, offset) {
        Some(SourceHoverFact::Type(fact)) => fact,
        _ => panic!("expected a type hover fact at offset {offset}"),
    }
}

fn store_root_fact_from(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &std::path::Path,
    offset: usize,
) -> StoreRootHoverFact {
    match source_hover_fact_at(snapshot, index, file, offset) {
        Some(SourceHoverFact::StoreRoot(fact)) => fact,
        _ => panic!("expected a store-root hover fact at offset {offset}"),
    }
}

/// Whether the resource hover fact carries a field member named `name` of rendered type `ty` — the
/// typed backing of a "required <name>: <ty>" member-summary line. The leaf path segment names the
/// member; deeper paths are nested groups the summary flattens.
fn resource_has_field(res: &SourceResourceHoverFact, name: &str, ty: &str) -> bool {
    res.members.iter().any(|member| {
        member.path.last().map(|seg| seg.name.as_str()) == Some(name)
            && matches!(&member.kind, SourceResourceHoverMemberKind::Field { ty: field_ty, .. } if field_ty == ty)
    })
}

fn operator_fact_from(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &std::path::Path,
    offset: usize,
) -> SourceOperatorHoverFact {
    match source_hover_fact_at(snapshot, index, file, offset) {
        Some(SourceHoverFact::Operator(fact)) => fact,
        _ => panic!("expected an operator hover fact at offset {offset}"),
    }
}

/// Whether the hover at `offset` resolves to the language-operator fact — the source of the rendered
/// "language operator" context. A segment merely spelled like an operator keyword must not.
fn is_operator_fact(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &std::path::Path,
    offset: usize,
) -> bool {
    matches!(
        source_hover_fact_at(snapshot, index, file, offset),
        Some(SourceHoverFact::Operator(_))
    )
}

fn is_std_library_fact(fact: &Option<SourceModulePathHoverFact>) -> bool {
    matches!(
        fact,
        Some(
            SourceModulePathHoverFact::StandardLibraryNamespace(_)
                | SourceModulePathHoverFact::StandardLibraryModule(_)
        )
    )
}

fn callable_fact_at(source: &str, offset: usize) -> SourceCallableHoverFact {
    let (snapshot, index, file) = analyzed_with_index(source);
    callable_fact_from(&snapshot, &index, &file, offset)
}

/// The intrinsic-callable signature at `needle`, panicking if the resolved callable is not an
/// intrinsic. The rendered "default library <x>" context line is a pure render of `sig.kind`, and
/// the signature code fence a pure render of `sig.path`/`params`/`return_shape`.
fn intrinsic_sig(source: &str, needle: &str) -> CallableSignature {
    match callable_fact(source, needle) {
        SourceCallableHoverFact::Intrinsic(sig) => sig,
        other => panic!("expected an intrinsic callable, got {other:?}"),
    }
}

/// The doc lines a callable hover fact carries, whichever callable variant it is. Asserting these
/// keeps a doc test on the typed `docs` payload instead of the Markdown layout that frames it.
fn callable_docs(fact: &SourceCallableHoverFact) -> &[String] {
    match fact {
        SourceCallableHoverFact::Intrinsic(sig) => &sig.docs,
        SourceCallableHoverFact::Function(f) => &f.docs,
        SourceCallableHoverFact::Parameter(p) => &p.docs,
        SourceCallableHoverFact::ModuleConst { docs, .. } => docs,
    }
}

/// Compare a rendered hover value to a reviewed golden under `tests/goldens/hover/<name>.md`.
/// Used only for the irreducible rendered-Markdown layout (effect blocks, member summaries, exact
/// multi-line signatures) that has no cleaner typed oracle. Set `MARROW_BLESS_GOLDENS=1` to rewrite
/// the golden after reviewing an intended semantic-contract change.
fn assert_hover_golden(name: &str, value: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/goldens/hover")
        .join(format!("{name}.md"));
    if std::env::var_os("MARROW_BLESS_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, value).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("missing hover golden `{name}`; set MARROW_BLESS_GOLDENS=1 to create it")
    });
    assert_eq!(
        value, expected,
        "hover golden `{name}` drifted; if this is an intended rendering/semantic change, review it \
         and rerun with MARROW_BLESS_GOLDENS=1 to update the golden"
    );
}

/// Assert the hover at `offset` resolves to the shared `Status` enum fact — the typed backing of
/// the "enum Status" signature, its "Lifecycle state." docs, and its member summary.
fn assert_status_enum_fact(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &std::path::Path,
    offset: usize,
) {
    let SourceSchemaHoverFact::Enum(en) = schema_fact_from(snapshot, index, file, offset) else {
        panic!("enum annotation must resolve to the enum schema fact");
    };
    assert_eq!(en.name, "Status");
    assert!(en.docs.iter().any(|line| line == "Lifecycle state."));
    assert!(
        en.members
            .iter()
            .any(|member| member.path.join("::") == "open")
    );
}

#[test]
fn hover_over_a_plain_parameter_use_shows_its_signature_fragment() {
    let source = "module a\n\npub fn f(n: int): int\n    return n\n";
    let (snapshot, file) = analyze(source);
    // Hover over the `n` in `return n`.
    let offset = offset_of(source, "return n") + "return ".len();
    let hover = hover(&snapshot, &file, offset).expect("a hover at the use of `n`");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup contents");
    };
    assert_eq!(markup.value, "```marrow\nn: int\n```");
}

#[test]
fn hover_over_a_second_parameter_use_shows_its_signature_fragment() {
    let source = "\
module a

pub fn f(first: int, second: string): string
    return second
";
    let (snapshot, file) = analyze(source);
    let offset = offset_of(source, "return second") + "return ".len();

    let hover = hover(&snapshot, &file, offset).expect("a hover at the use of `second`");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup contents");
    };
    assert_eq!(markup.value, "```marrow\nsecond: string\n```");
}

#[test]
fn hover_over_a_parameter_use_in_function_body_shows_its_signature_fragment() {
    let source = "\
module a

pub fn fill(value: int): int
    return value
";
    let (snapshot, file) = analyze(source);
    let offset = offset_of(source, "return value") + "return ".len();

    let hover = hover(&snapshot, &file, offset).expect("a hover at the use of `value`");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup contents");
    };
    assert_eq!(markup.value, "```marrow\nvalue: int\n```");
}

#[test]
fn hover_over_a_parameter_declaration_does_not_show_parameter_use_hover() {
    let source = "module a\n\npub fn f(n: int): int\n    return n\n";
    let (snapshot, file) = analyze(source);
    let offset = offset_of(source, "n: int");

    let hover = hover(&snapshot, &file, offset);
    if let Some(hover) = hover {
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert_ne!(markup.value, "```marrow\nn: int\n```");
    }
}

#[test]
fn hover_over_a_field_named_like_a_parameter_does_not_show_parameter_use_hover() {
    let source = "\
module a

resource Book
    required title: string

pub fn get(
    ;; The parameter, not the field.
    title: string,
): string
    var book: Book
    return book.title
";
    let (snapshot, file) = analyze(source);
    let index = build_binding_index(&snapshot);
    let offset = offset_of(source, "book.title") + "book.".len();

    // A field use that merely shares a parameter's name must not resolve to the parameter hover,
    // which is what would leak the parameter's docs.
    assert!(
        !matches!(
            source_hover_fact_at(&snapshot, &index, &file, offset),
            Some(SourceHoverFact::Callable(
                SourceCallableHoverFact::Parameter(_)
            ))
        ),
        "field hover must not resolve to the same-named parameter"
    );
}

#[test]
fn hover_over_a_named_argument_label_does_not_show_parameter_use_hover() {
    let source = "\
module a

resource Book
    required title: string

pub fn make(
    ;; The parameter, not the named argument label.
    title: string,
): Book
    return Book(title: title)
";
    let (snapshot, file) = analyze(source);
    let index = build_binding_index(&snapshot);
    let offset = offset_of(source, "Book(title: title)") + "Book(".len();

    // The named-argument label is not a use of the parameter, so it must not resolve to the
    // parameter hover (which would inherit the parameter's docs).
    assert!(
        !matches!(
            source_hover_fact_at(&snapshot, &index, &file, offset),
            Some(SourceHoverFact::Callable(
                SourceCallableHoverFact::Parameter(_)
            ))
        ),
        "named argument label must not resolve to the same-named parameter"
    );
}

#[test]
fn hover_over_a_duplicate_parameter_name_uses_the_latest_binding() {
    let source = "\
module a

pub fn f(
    ;; First parameter docs.
    n: int,
    ;; Second parameter docs.
    n: string,
): string
    return n
";
    let offset = offset_of(source, "return n") + "return ".len();

    let SourceCallableHoverFact::Parameter(param) = callable_fact_at(source, offset) else {
        panic!("expected a parameter hover");
    };
    assert_eq!(param.name, "n");
    // The docs of the binding in scope — the later `n` — and not the shadowed first parameter.
    assert_eq!(param.docs, ["Second parameter docs."]);
}

#[test]
fn hover_over_a_documented_parameter_use_shows_its_docs() {
    let source = "\
module a

pub fn f(
    ;; The number to return.
    n: int,
): int
    return n
";
    let offset = offset_of(source, "return n") + "return ".len();

    let SourceCallableHoverFact::Parameter(param) = callable_fact_at(source, offset) else {
        panic!("expected a parameter hover");
    };
    assert_eq!(param.name, "n");
    assert_eq!(param.docs, ["The number to return."]);
}

#[test]
fn hover_over_a_shadowing_local_use_does_not_show_parameter_use_docs() {
    let source = "\
module a

pub fn f(
    ;; The outer parameter.
    n: int,
): int
    if true
        const n: int = 99
        return n
    return n
";
    let (snapshot, file) = analyze(source);
    let offset = offset_of(source, "return n") + "return ".len();

    let hover = hover(&snapshot, &file, offset).expect("a hover at the local use of `n`");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup contents");
    };
    // The exact value is a bare type with no docs section, which already proves the shadowing
    // local does not inherit the outer parameter's docs.
    assert_eq!(markup.value, "```marrow\nint\n```");
}

#[test]
fn hover_over_a_function_call_shows_signature_and_description() {
    let source = "\
module a

;; Adds two numbers.
pub fn add(x: int, y: int): int
    return x + y

pub fn caller(): int
    return add(1, 2)
";
    let offset = source.rfind("add(1, 2)").unwrap();

    let SourceCallableHoverFact::Function(func) = callable_fact_at(source, offset) else {
        panic!("expected a function hover");
    };
    assert_eq!(func.name, "add");
    assert_eq!(func.docs, ["Adds two numbers."]);
    // No parameter carries docs, which is why the rendered hover omits the parameter section.
    assert!(func.params.iter().all(|param| param.docs.is_empty()));
}

#[test]
fn hover_over_a_function_call_shows_direct_effects_from_checked_facts() {
    let source = "\
module a

resource Book
    required title: string
    required visits: int

store ^books(id: int): Book

pub fn touch(id: int): string
    const title: string = ^books(id).title ?? \"\"
    const visits: int = ^books(id).visits ?? 0
    transaction
        ^books(id).visits = visits + 1
    print(title)
    return title

pub fn caller(): string
    return touch(1)
";
    // The direct-effect names (Book.title, Book.visits) exist only in the rendered form: the typed
    // `DirectEffectFacts` carries opaque resource/member ids, so the resolved-name rendering is the
    // readable oracle. A typed presence check backs it; the reviewed golden pins the resolved names.
    let SourceCallableHoverFact::Function(func) = callable_fact(source, "touch(1)") else {
        panic!("expected a function hover");
    };
    let effects = func
        .direct_effects
        .expect("the function fact carries direct effects");
    assert!(effects.transactions);
    assert!(!effects.saved_reads.is_empty());
    assert!(!effects.saved_writes.is_empty());
    assert!(!effects.host_calls.is_empty());

    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let value = hover_value(&snapshot, &index, &file, source, "touch(1)");
    assert_hover_golden("function_call_direct_effects", &value);
}

#[test]
fn hover_over_a_function_declaration_name_shows_signature() {
    let source = "\
module a

pub fn answer(): int
    return 42
";
    let (snapshot, file) = analyze(source);
    let offset = offset_of(source, "answer") + 1;

    let hover = hover(&snapshot, &file, offset).expect("a hover at the function declaration");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup contents");
    };
    assert_eq!(markup.value, "```marrow\nfn answer(): int\n```");
}

#[test]
fn hover_over_bare_builtin_call_shows_default_library_signature() {
    let source = "\
module a

resource Book
    required title: string

store ^books(id: int): Book

pub fn f(): bool
    return exists(^books(1))

pub fn g(): Id(^books)
    return nextId(^books)

pub fn h()
    print(\"b\")
    return count(^books)
";
    // "default library builtin." is a pure render of `sig.kind == Builtin`; the signature code
    // fence a render of `sig.path`; the docs are the typed `sig.docs` payload.
    let exists = intrinsic_sig(source, "exists(^books");
    assert_eq!(exists.kind, CallableSignatureKind::Builtin);
    assert_eq!(exists.path, ["exists"]);
    assert!(
        exists
            .docs
            .iter()
            .any(|line| line == "Returns true when the saved path exists.")
    );

    let next_id = callable_fact(source, "nextId(^books");
    let next_id_docs = callable_docs(&next_id);
    assert!(
        next_id_docs
            .iter()
            .any(|line| line == "Returns the next id for a saved root.")
    );
    // Docs are specific to the hovered builtin, not inherited from a sibling.
    assert!(
        !next_id_docs
            .iter()
            .any(|line| line == "Returns true when the saved path exists.")
    );

    let print = callable_fact(source, "print(\"b");
    assert!(
        callable_docs(&print)
            .iter()
            .any(|line| line == "Writes rendered text to output with a newline.")
    );
    let count = callable_fact(source, "count(^books");
    assert!(
        callable_docs(&count).iter().any(|line| line
            == "Returns child count for a saved path, 1 for a scalar, or 0 when absent.")
    );
}

#[test]
fn hover_over_std_call_leaf_shows_signature_and_context() {
    let source = "\
module a

pub fn f(): int
    return std::text::length(\"abc\")
";
    let sig = intrinsic_sig(source, "length(\"abc");
    assert_eq!(sig.kind, CallableSignatureKind::StandardLibrary);
    assert_eq!(sig.path, ["std", "text", "length"]);
}

#[test]
fn hover_over_imported_std_call_leaf_uses_contextual_callable_signature() {
    let source = "\
module a

use std::text

pub fn f(): int
    return text::length(\"abc\")
";
    // The imported `text::length` leaf expands to the canonical `std::text::length` path (a
    // StandardLibrary intrinsic) from the file's import context.
    let sig = intrinsic_sig(source, "length(\"abc");
    assert_eq!(sig.kind, CallableSignatureKind::StandardLibrary);
    assert_eq!(sig.path, ["std", "text", "length"]);
}

#[test]
fn hover_over_scalar_conversion_call_shows_default_library_signature() {
    let source = "\
module a

pub fn f(): int
    return int(\"1\")
";
    let sig = intrinsic_sig(source, "int(\"1");
    assert_eq!(sig.kind, CallableSignatureKind::ScalarConversion);
    assert_eq!(sig.path, ["int"]);
}

#[test]
fn hover_over_error_code_conversion_shows_default_library_signature() {
    let source = "\
module a

pub fn f(): ErrorCode
    return ErrorCode(\"not_found\")
";
    // "default library scalar conversion." renders the ScalarConversion intrinsic kind; ErrorCode is
    // a scalar-conversion constructor over the language `ErrorCode` type.
    let sig = intrinsic_sig(source, "ErrorCode(\"not");
    assert_eq!(sig.kind, CallableSignatureKind::ScalarConversion);
    assert_eq!(sig.path, ["ErrorCode"]);
}

#[test]
fn hover_over_std_namespace_and_module_segments_shows_default_library_context() {
    let source = "\
module a

pub fn f(): int
    return std::text::length(\"abc\")
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);

    // "default library namespace." renders the StandardLibraryNamespace variant; the module
    // segment renders StandardLibraryModule and summarizes its typed `operations` list.
    let std_fact = module_path_fact_at(&snapshot, &index, &file, offset_of(source, "std::text"));
    assert!(matches!(
        std_fact,
        Some(SourceModulePathHoverFact::StandardLibraryNamespace(_))
    ));

    let text_offset = offset_of(source, "std::text") + "std::".len();
    let text_fact = module_path_fact_at(&snapshot, &index, &file, text_offset);
    let Some(SourceModulePathHoverFact::StandardLibraryModule(module)) = text_fact else {
        panic!("expected a std::text module fact");
    };
    assert_eq!(module.module, "text");
    assert!(module.operations.iter().any(|op| op.name == "length"));

    let length_sig = intrinsic_sig(source, "length(\"abc");
    assert_eq!(length_sig.path, ["std", "text", "length"]);
}

#[test]
fn hover_over_std_namespace_and_module_segments_requires_std_operation() {
    let std_text_source = "\
module std::text

pub fn custom(): int
    return 1
";
    let std_custom_source = "\
module std::custom

pub fn ping(): int
    return 1
";
    let app_source = "\
module app

pub fn run(): int
    const text_value = std::text::custom()
    const custom_value = std::custom::ping()
    return text_value
";
    let (snapshot, file) = analyze_files(
        &[
            ("std/text.mw", std_text_source),
            ("std/custom.mw", std_custom_source),
            ("app.mw", app_source),
        ],
        "app.mw",
    );
    let index = index_for(&snapshot);

    for needle in ["std::text::custom", "std::custom::ping"] {
        let std_seg = module_path_fact_at(&snapshot, &index, &file, offset_of(app_source, needle));
        assert!(
            !is_std_library_fact(&std_seg),
            "project-owned {needle} std segment must not resolve to a default-library fact"
        );

        let module_offset = offset_of(app_source, needle) + "std::".len();
        let module_seg = module_path_fact_at(&snapshot, &index, &file, module_offset);
        assert!(
            !is_std_library_fact(&module_seg),
            "project-owned {needle} module segment must not resolve to a default-library fact"
        );
    }
}

#[test]
fn hover_over_std_namespace_and_module_segments_keep_builtin_precedence() {
    let std_text_source = "\
module std::text

pub fn length(value: string): bool
    return false
";
    let app_source = "\
module app

pub fn run(): int
    return std::text::length(\"abc\")
";
    let (snapshot, file) = analyze_files(
        &[("std/text.mw", std_text_source), ("app.mw", app_source)],
        "app.mw",
    );
    let index = index_for(&snapshot);

    let std_seg = module_path_fact_at(&snapshot, &index, &file, offset_of(app_source, "std::text"));
    assert!(
        matches!(
            std_seg,
            Some(SourceModulePathHoverFact::StandardLibraryNamespace(_))
        ),
        "std segment keeps the default-library namespace fact when a builtin std op wins dispatch"
    );

    let text_offset = offset_of(app_source, "std::text") + "std::".len();
    let text_seg = module_path_fact_at(&snapshot, &index, &file, text_offset);
    assert!(
        matches!(
            text_seg,
            Some(SourceModulePathHoverFact::StandardLibraryModule(_))
        ),
        "std::text segment keeps the default-library module fact when a builtin std op wins dispatch"
    );

    // The leaf keeps builtin precedence over the project function: it resolves to the StandardLibrary
    // intrinsic `std::text::length`, not the project's `length`.
    let length_sig = match callable_fact_from(
        &snapshot,
        &index,
        &file,
        offset_of(app_source, "length(\"abc"),
    ) {
        SourceCallableHoverFact::Intrinsic(sig) => sig,
        other => panic!("expected an intrinsic callable, got {other:?}"),
    };
    assert_eq!(length_sig.kind, CallableSignatureKind::StandardLibrary);
    assert_eq!(length_sig.path, ["std", "text", "length"]);
}

#[test]
fn hover_over_project_module_leaf_in_use_shows_canonical_module_fact() {
    let books_source = "\
module shelf::books

pub fn titleOf(): string
    return \"Dune\"
";
    let app_source = "\
module shelf::app

use shelf::books

pub fn run(): string
    return books::titleOf()
";
    let (snapshot, file) = analyze_files(
        &[
            ("shelf/books.mw", books_source),
            ("shelf/app.mw", app_source),
        ],
        "shelf/app.mw",
    );
    let index = index_for(&snapshot);
    let leaf = offset_of(app_source, "use shelf::books") + "use shelf::".len();
    // "project source module" renders the ProjectModule variant, which carries the canonical module
    // name but no function list — so a member like `titleOf` cannot leak into the module hover.
    let fact = module_path_fact_at(&snapshot, &index, &file, leaf + 1);
    let Some(SourceModulePathHoverFact::ProjectModule(module)) = fact else {
        panic!("expected a project-module hover fact");
    };
    assert_eq!(module.module, "shelf::books");
}

#[test]
fn hover_over_project_module_qualifier_keeps_function_leaf_hover() {
    let books_source = "\
module shelf::books

;; Formats a book title.
pub fn titleOf(): string
    return \"Dune\"
";
    let app_source = "\
module shelf::app

use shelf::books

pub fn run(): string
    return books::titleOf()
";
    let (snapshot, file) = analyze_files(
        &[
            ("shelf/books.mw", books_source),
            ("shelf/app.mw", app_source),
        ],
        "shelf/app.mw",
    );
    let index = index_for(&snapshot);
    let call = offset_of(app_source, "books::titleOf()");
    let module =
        hover_value_at(&snapshot, &index, &file, call + 1).expect("a hover over the module prefix");
    assert!(
        module.starts_with("```marrow\nmodule shelf::books\n```"),
        "module-prefix hover should render the canonical module fact: {module}"
    );
    // The module prefix resolves to the ProjectModule variant, which carries no function docs.
    assert!(matches!(
        module_path_fact_at(&snapshot, &index, &file, call + 1),
        Some(SourceModulePathHoverFact::ProjectModule(_))
    ));

    let leaf = call + "books::".len();
    let function =
        hover_value_at(&snapshot, &index, &file, leaf + 1).expect("a hover over the function leaf");
    assert!(
        function.starts_with("```marrow\nfn titleOf(): string\n```"),
        "function leaf should keep the rich function hover: {function}"
    );
    assert!(
        callable_docs(&callable_fact_from(&snapshot, &index, &file, leaf + 1))
            .iter()
            .any(|line| line == "Formats a book title.")
    );
}

#[test]
fn hover_over_fully_qualified_project_module_segment_keeps_function_leaf_hover() {
    let books_source = "\
module shelf::books

;; Formats a book title.
pub fn titleOf(): string
    return \"Dune\"
";
    let app_source = "\
module shelf::app

pub fn run(): string
    return shelf::books::titleOf()
";
    let (snapshot, file) = analyze_files(
        &[
            ("shelf/books.mw", books_source),
            ("shelf/app.mw", app_source),
        ],
        "shelf/app.mw",
    );
    let index = index_for(&snapshot);
    let call = offset_of(app_source, "shelf::books::titleOf()");
    let books = call + "shelf::".len();
    let module = hover_value_at(&snapshot, &index, &file, books + 1)
        .expect("a hover over the fully qualified module segment");
    assert!(
        module.starts_with("```marrow\nmodule shelf::books\n```"),
        "fully qualified module-prefix hover should render the canonical module fact: {module}"
    );

    let leaf = call + "shelf::books::".len();
    let function =
        hover_value_at(&snapshot, &index, &file, leaf + 1).expect("a hover over the function leaf");
    assert!(
        function.starts_with("```marrow\nfn titleOf(): string\n```"),
        "fully qualified function leaf should keep the function hover: {function}"
    );
    assert!(
        callable_docs(&callable_fact_from(&snapshot, &index, &file, leaf + 1))
            .iter()
            .any(|line| line == "Formats a book title.")
    );
}

#[test]
fn hover_over_project_module_qualifier_does_not_show_qualified_enum_hover() {
    let state_source = "\
module shelf::state

;; Publication state.
pub enum Status
    ;; Open for edits.
    open
";
    let app_source = "\
module shelf::app

use shelf::state

pub fn status(): state::Status
    return state::Status::open
";
    let (snapshot, file) = analyze_files(
        &[
            ("shelf/state.mw", state_source),
            ("shelf/app.mw", app_source),
        ],
        "shelf/app.mw",
    );
    let index = index_for(&snapshot);
    let qualifier = offset_of(app_source, "state::Status::open");
    let value = hover_value_at(&snapshot, &index, &file, qualifier + 1)
        .expect("a hover over the module qualifier");
    assert!(
        value.starts_with("```marrow\nmodule shelf::state\n```"),
        "qualified enum module-prefix should render the module fact: {value}"
    );
    assert!(
        !value.starts_with("```marrow\nenum Status\n```"),
        "qualified enum module-prefix should not show the enum hover: {value}"
    );
}

#[test]
fn hover_over_enum_path_head_wins_over_same_named_project_module() {
    let status_source = "\
module status

pub fn code(): int
    return 1
";
    let app_source = "\
module app

;; Local status enum.
enum status
    active

pub fn current(): status
    return status::active
";
    let (snapshot, file) = analyze_files(
        &[("status.mw", status_source), ("app.mw", app_source)],
        "app.mw",
    );
    let index = index_for(&snapshot);
    let head = offset_of(app_source, "status::active");
    let value = hover_value_at(&snapshot, &index, &file, head + 1)
        .expect("a hover over the enum path head");

    assert!(
        value.starts_with("```marrow\nenum status\n```"),
        "enum path head should keep the enum hover: {value}"
    );
    // The head resolves to the enum schema fact (not the same-named module), carrying the enum docs.
    let SourceSchemaHoverFact::Enum(enum_fact) =
        schema_fact_from(&snapshot, &index, &file, head + 1)
    else {
        panic!("expected an enum schema hover fact");
    };
    assert_eq!(enum_fact.name, "status");
    assert!(
        enum_fact
            .docs
            .iter()
            .any(|line| line == "Local status enum.")
    );
}

#[test]
fn hover_over_removed_resource_identity_head_does_not_invent_identity_hover() {
    let book_source = "\
module book

pub fn code(): int
    return 1
";
    let app_source = "\
module app

;; Saved books.
resource book
    required title: string

store ^books(id: int): book

pub fn load(): book::Id
    return book::Id(1)
";
    let (snapshot, file) = analyze_files(
        &[("book.mw", book_source), ("app.mw", app_source)],
        "app.mw",
    );
    let index = index_for(&snapshot);
    let head = offset_of(app_source, "book::Id(1)");
    // A removed resource identity head must not resolve to the resource schema hover, which is the
    // only source of the resource's docs.
    assert!(
        !matches!(
            source_hover_fact_at(&snapshot, &index, &file, head + 1),
            Some(SourceHoverFact::Schema(_))
        ),
        "removed identity path must not inherit resource schema docs"
    );
    if let Some(value) = hover_value_at(&snapshot, &index, &file, head + 1) {
        assert!(
            !value.starts_with("```marrow\nbook::Id\n```"),
            "removed identity path must not get a generated identity hover: {value}"
        );
    }
}

#[test]
fn hover_over_project_std_text_module_without_builtin_dispatch_shows_project_module() {
    let std_text_source = "\
module std::text

;; Project-only text helper.
pub fn custom(): int
    return 1
";
    let app_source = "\
module app

pub fn run(): int
    return std::text::custom()
";
    let (snapshot, file) = analyze_files(
        &[("std/text.mw", std_text_source), ("app.mw", app_source)],
        "app.mw",
    );
    let index = index_for(&snapshot);
    let text = offset_of(app_source, "std::text::custom") + "std::".len();
    let module = hover_value_at(&snapshot, &index, &file, text + 1)
        .expect("a hover over the project std::text module segment");
    assert!(
        module.starts_with("```marrow\nmodule std::text\n```"),
        "project std::text module-prefix hover should render the project module fact: {module}"
    );
    // A project-owned std::text resolves to the ProjectModule variant, never a default-library fact.
    assert!(matches!(
        module_path_fact_at(&snapshot, &index, &file, text + 1),
        Some(SourceModulePathHoverFact::ProjectModule(_))
    ));

    let custom_offset = offset_of(app_source, "custom()") + 1;
    let custom = hover_value_at(&snapshot, &index, &file, custom_offset)
        .expect("a hover over the project function leaf");
    assert!(
        custom.starts_with("```marrow\nfn custom(): int\n```"),
        "project function leaf should keep its function hover: {custom}"
    );
    // The leaf is a project Function (not a default-library Intrinsic) carrying its own docs.
    let SourceCallableHoverFact::Function(custom_fact) =
        callable_fact_from(&snapshot, &index, &file, custom_offset)
    else {
        panic!("expected a project function hover, not a default-library intrinsic");
    };
    assert!(
        custom_fact
            .docs
            .iter()
            .any(|line| line == "Project-only text helper.")
    );
}

#[test]
fn hover_over_std_text_module_declaration_shows_project_module() {
    let source = "\
module std::text

pub fn custom(): int
    return 1
";
    let (snapshot, file) = analyze_files(&[("std/text.mw", source)], "std/text.mw");
    let index = index_for(&snapshot);
    let text = offset_of(source, "module std::text") + "module std::".len();
    let module = hover_value_at(&snapshot, &index, &file, text + 1)
        .expect("a hover over the std::text declaration segment");

    assert!(
        module.starts_with("```marrow\nmodule std::text\n```"),
        "module declaration hover should render the project module fact: {module}"
    );
    assert!(matches!(
        module_path_fact_at(&snapshot, &index, &file, text + 1),
        Some(SourceModulePathHoverFact::ProjectModule(_))
    ));
}

#[test]
fn hover_over_expression_operators_shows_language_operator_docs() {
    let source = "\
module a

pub fn sum(x: int, y: int): int
    return x + y

pub fn both(left: bool, right: bool): bool
    return left and right
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);

    // "language operator" renders the SourceOperator fact; the behavior wording is its typed
    // `description` field, and the operator symbol its `spelling`.
    let plus = operator_fact_from(&snapshot, &index, &file, offset_of(source, "+"));
    assert_eq!(plus.spelling, "+");
    assert!(plus.description.contains("addition"));

    let and_op = operator_fact_from(&snapshot, &index, &file, offset_of(source, "and"));
    assert_eq!(and_op.spelling, "and");
    assert!(and_op.description.contains("logical"));
}

#[test]
fn hover_over_operator_keyword_path_segment_does_not_show_operator_docs() {
    let ops_source = "\
module and::tools

pub fn length(value: string): int
    return 0
";
    let app_source = "\
module app

pub fn run(): int
    return and::tools::length(\"abc\")
";
    let (snapshot, file) = analyze_files(
        &[("and/tools.mw", ops_source), ("app.mw", app_source)],
        "app.mw",
    );
    let index = index_for(&snapshot);

    let module_offset = offset_of(app_source, "and::tools");
    assert!(
        !is_operator_fact(&snapshot, &index, &file, module_offset),
        "a module path segment merely spelled like an operator must not resolve to the operator fact"
    );
}

#[test]
fn hover_over_operator_keyword_declaration_and_import_does_not_show_operator_docs() {
    let and_source = "\
module and

pub fn value(): int
    return 1
";
    let app_source = "\
module app

use and

pub fn run(): int
    return and::value()
";
    let (module_snapshot, module_file) = analyze_files(&[("and.mw", and_source)], "and.mw");
    let module_index = index_for(&module_snapshot);
    assert!(
        !is_operator_fact(
            &module_snapshot,
            &module_index,
            &module_file,
            offset_of(and_source, "module and") + "module ".len(),
        ),
        "a module declaration segment spelled like an operator must not resolve to the operator fact"
    );

    let (app_snapshot, app_file) =
        analyze_files(&[("and.mw", and_source), ("app.mw", app_source)], "app.mw");
    let app_index = index_for(&app_snapshot);
    assert!(
        !is_operator_fact(
            &app_snapshot,
            &app_index,
            &app_file,
            offset_of(app_source, "use and") + "use ".len(),
        ),
        "a use segment spelled like an operator must not resolve to the operator fact"
    );
}

#[test]
fn hover_over_operator_keyword_resource_fields_does_not_show_operator_docs() {
    let source = "\
module app

resource Thing
    required and: int
    required or: int
    required is: bool
    required not: bool

store ^things(id: int): Thing
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);

    for name in ["and", "or", "is", "not"] {
        let offset = offset_of(source, &format!("required {name}:")) + "required ".len();
        assert!(
            !is_operator_fact(&snapshot, &index, &file, offset),
            "resource field named `{name}` must not resolve to the language-operator fact"
        );
    }
}

#[test]
fn builtin_call_hover_wins_over_same_named_user_function_but_not_declaration() {
    let source = "\
module a

resource Book
    required title: string

store ^books(id: int): Book

fn exists(value: unknown): bool
    return false

fn f(): bool
    return exists(^books(1))
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);

    let declaration_offset = offset_of(source, "fn exists") + "fn ".len();
    let declaration = hover_value_at(&snapshot, &index, &file, declaration_offset)
        .expect("a hover at the user function declaration");
    // The exact user-function value already proves the declaration keeps the user function and gets
    // no builtin context.
    assert_eq!(
        declaration, "```marrow\nfn exists(value: unknown): bool\n```",
        "declaration hover should remain the user function"
    );

    // The call resolves to the default-library builtin, not the same-named user function.
    let call = intrinsic_sig(source, "exists(^books");
    assert_eq!(call.kind, CallableSignatureKind::Builtin);
    assert_eq!(call.path, ["exists"]);
}

#[test]
fn qualified_builtin_like_leaves_do_not_get_default_library_hover() {
    let foo_source = "\
module foo

pub fn exists(): bool
    return false
";
    let foo_std_text_source = "\
module foo::std::text

pub fn length(value: string): int
    return 0
";
    let app_source = "\
module app

use foo
use foo::std::text

pub fn f(): bool
    const found = foo::exists()
    const text_len = foo::std::text::length(\"abc\")
    return found
";
    let (snapshot, file) = analyze_files(
        &[
            ("foo.mw", foo_source),
            ("foo/std/text.mw", foo_std_text_source),
            ("app.mw", app_source),
        ],
        "app.mw",
    );
    let index = index_for(&snapshot);

    // The qualified `foo::exists` and `foo::std::text::length` resolve to the project functions, not
    // the default-library intrinsics they are spelled like.
    assert!(
        matches!(
            callable_fact_from(&snapshot, &index, &file, offset_of(app_source, "exists()")),
            SourceCallableHoverFact::Function(_)
        ),
        "qualified foo::exists must resolve to the project function, not the builtin"
    );
    assert!(
        matches!(
            callable_fact_from(
                &snapshot,
                &index,
                &file,
                offset_of(app_source, "length(\"abc")
            ),
            SourceCallableHoverFact::Function(_)
        ),
        "qualified foo::std::text::length must resolve to the project function, not the std op"
    );

    let std_offset = offset_of(app_source, "std::text::length");
    assert!(
        !is_std_library_fact(&module_path_fact_at(&snapshot, &index, &file, std_offset)),
        "the qualified foo::std segment must not resolve to a default-library fact"
    );
    let text_offset = offset_of(app_source, "std::text::length") + "std::".len();
    assert!(
        !is_std_library_fact(&module_path_fact_at(&snapshot, &index, &file, text_offset)),
        "the qualified foo::std::text segment must not resolve to a default-library fact"
    );
}

#[test]
fn hover_over_a_single_letter_function_declaration_name_ignores_the_fn_keyword() {
    let source = "\
module a

pub fn n(): int
    return 1
";
    let (snapshot, file) = analyze(source);
    let offset = offset_of(source, "n():");

    let hover = hover(&snapshot, &file, offset).expect("a hover at the function declaration");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup contents");
    };
    assert_eq!(markup.value, "```marrow\nfn n(): int\n```");
}

#[test]
fn hover_over_a_function_with_multiple_params_shows_signature() {
    let source = "\
module a

pub fn fill(value: int, count: int): int
    return value + count
";
    let (snapshot, file) = analyze(source);
    let offset = offset_of(source, "fill") + 1;

    let hover = hover(&snapshot, &file, offset).expect("a hover at the function declaration");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup contents");
    };
    assert_eq!(
        markup.value,
        "```marrow\nfn fill(value: int, count: int): int\n```"
    );
}

#[test]
fn hover_over_a_function_with_parameter_docs_shows_documented_params() {
    let source = "\
module a

pub fn add(
    ;; The left value.
    x: int,
    ;; The right value.
    y: int,
): int
    return x + y

pub fn caller(): int
    return add(1, 2)
";
    let offset = source.rfind("add(1, 2)").unwrap();

    // Each parameter's docs are the typed `SourceCallableParamFact::docs`; the rendered
    // "**Parameters**" section and "- `x`: ..." bullets are a pure layout of those payloads.
    let SourceCallableHoverFact::Function(func) = callable_fact_at(source, offset) else {
        panic!("expected a function hover");
    };
    let x = func.params.iter().find(|param| param.name == "x").unwrap();
    assert_eq!(x.docs, ["The left value."]);
    let y = func.params.iter().find(|param| param.name == "y").unwrap();
    assert_eq!(y.docs, ["The right value."]);
}

#[test]
fn hover_over_a_resource_declaration_name_shows_docs_and_members_without_store_identity() {
    let source = "\
module a

;; Book records.
resource Book
    ;; Display title.
    required title: string
    code: ErrorCode
    pages: int
    tags(pos: int): string

;; Books saved by id.
store ^books(id: int): Book
    index byTitle(title) unique
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "Book\n    ;; Display");
    // The docs and field types are typed payloads; a resource fact carries no store docs, identity
    // type, or index, so those cannot appear. The exact member-summary layout (field-type spelling,
    // keyed-leaf rendering, syntax order) is a pure render, pinned by the reviewed golden.
    let SourceSchemaHoverFact::Resource(res) = schema_fact_from(&snapshot, &index, &file, offset)
    else {
        panic!("expected the resource schema fact");
    };
    assert_eq!(res.name, "Book");
    assert!(res.docs.iter().any(|line| line == "Book records."));
    assert!(resource_has_field(&res, "title", "string"));
    assert!(resource_has_field(&res, "code", "ErrorCode"));

    let value = hover_value(&snapshot, &index, &file, source, "Book\n    ;; Display");
    assert_hover_golden(
        "resource_declaration_members_without_store_identity",
        &value,
    );
}

#[test]
fn hover_over_a_store_index_declaration_name_shows_docs_and_signature() {
    let source = "\
module a

resource Book
    required title: string

store ^books(id: int): Book
    ;; Books by display title.
    index byTitle(title) unique
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let value = hover_value(&snapshot, &index, &file, source, "byTitle");

    assert_eq!(
        value,
        "```marrow\nindex byTitle(title) unique\n```\n\nBooks by display title."
    );
}

#[test]
fn hover_over_the_resource_keyword_does_not_show_the_resource() {
    let source = "\
module a

resource Book
    required title: string

;; Books saved by id.
store ^books(id: int): Book
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "resource Book");
    let hover = hover_with_index(&snapshot, &index, &file, offset);
    if let Some(Hover {
        contents: HoverContents::Markup(markup),
        ..
    }) = hover
    {
        assert!(
            !markup.value.starts_with("```marrow\nresource Book\n```"),
            "resource keyword should not inherit the declaration-name hover: {}",
            markup.value
        );
    }
}

#[test]
fn hover_over_a_resource_constructor_use_is_rich() {
    let source = "\
module a

;; Books saved by id.
resource Book
    required title: string

pub fn make(): Book
    return Book(title: \"x\")
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "Book(title");
    let SourceSchemaHoverFact::Resource(res) = schema_fact_from(&snapshot, &index, &file, offset)
    else {
        panic!("constructor should resolve to the resource schema, not only the type");
    };
    assert_eq!(res.name, "Book");
    assert!(res.docs.iter().any(|line| line == "Books saved by id."));
    assert!(resource_has_field(&res, "title", "string"));
}

#[test]
fn hover_over_a_qualified_resource_constructor_leaf_is_rich() {
    let state_source = "\
module shelf::state

;; Books from state.
resource Book
    required title: string
";
    let app_source = "\
module shelf::app

use shelf::state

pub fn make()
    var book = state::Book(title: \"x\")
";
    let (snapshot, file) = analyze_files(
        &[
            ("shelf/state.mw", state_source),
            ("shelf/app.mw", app_source),
        ],
        "shelf/app.mw",
    );
    let index = index_for(&snapshot);
    let call = offset_of(app_source, "state::Book(title");
    let qualifier_hover = hover_with_index(&snapshot, &index, &file, call + 1);
    if let Some(Hover {
        contents: HoverContents::Markup(markup),
        ..
    }) = qualifier_hover
    {
        assert!(
            !markup.value.starts_with("```marrow\nresource Book\n```"),
            "module qualifier should not pretend to be the resource: {}",
            markup.value
        );
    }

    let leaf_offset = call + "state::".len();
    let hover = hover_with_index(&snapshot, &index, &file, leaf_offset).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    let value = markup.value;

    assert!(
        value.starts_with("```marrow\nresource Book\n```"),
        "qualified resource constructor leaf should show resource hover: {value}"
    );
    let SourceSchemaHoverFact::Resource(res) =
        schema_fact_from(&snapshot, &index, &file, leaf_offset)
    else {
        panic!("expected a resource schema fact at the constructor leaf");
    };
    assert!(res.docs.iter().any(|line| line == "Books from state."));
    assert!(resource_has_field(&res, "title", "string"));
}

#[test]
fn hover_over_aligned_cross_file_qualified_resource_constructor_leaf_is_rich() {
    let state_base = "\
module shelf::state

;; Books from state.
resource Book
    required title: string
";
    let app_source = "\
module shelf::app
use shelf::state
pub fn make()
  state::Book(title: \"x\")
";
    let leaf = offset_of(app_source, "state::Book(title") + "state::".len();
    let declaration = offset_of(state_base, "resource Book") + "resource ".len();
    let padding = "x".repeat(
        leaf.checked_sub(declaration)
            .expect("call leaf is after declaration name in this fixture"),
    );
    let docs = format!("Books from state.{padding}");
    let state_source = format!(
        "\
module shelf::state

;; {docs}
resource Book
    required title: string
"
    );
    assert_eq!(
        offset_of(&state_source, "resource Book") + "resource ".len(),
        leaf
    );

    let (snapshot, file) = analyze_files(
        &[
            ("shelf/state.mw", state_source.as_str()),
            ("shelf/app.mw", app_source),
        ],
        "shelf/app.mw",
    );
    let index = index_for(&snapshot);
    let qualifier = offset_of(app_source, "state::Book(title");
    let qualifier_hover = hover_value_at(&snapshot, &index, &file, qualifier + 1);
    assert!(
        !qualifier_hover
            .as_deref()
            .is_some_and(|value| value.starts_with("```marrow\nresource Book\n```")),
        "module qualifier should not pretend to be the resource: {qualifier_hover:?}"
    );

    let value =
        hover_value_at(&snapshot, &index, &file, leaf).expect("resource hover on aligned leaf");
    assert!(
        value.starts_with("```marrow\nresource Book\n```"),
        "qualified resource constructor leaf should show resource hover: {value}"
    );
    let SourceSchemaHoverFact::Resource(res) = schema_fact_from(&snapshot, &index, &file, leaf)
    else {
        panic!("expected a resource schema fact at the aligned leaf");
    };
    assert!(res.docs.iter().any(|line| line == &docs));
    assert!(resource_has_field(&res, "title", "string"));
}

#[test]
fn hover_over_resource_type_annotation_shows_resource_hover() {
    let source = "\
module a

;; Books saved by id.
resource Book
    required title: string

pub fn echo(book: Book): Book
    var local: Book
    return book
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);

    for needle in ["book: Book", "): Book", "local: Book"] {
        let offset = offset_of(source, needle) + needle.rfind("Book").unwrap();
        let hover = hover_with_index(&snapshot, &index, &file, offset).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        assert!(
            markup.value.starts_with("```marrow\nresource Book\n```"),
            "resource annotation hover should lead with resource signature: {}",
            markup.value
        );
        // A resource hover fact has docs and members but no store-owned identity type, so it cannot
        // name `Id(^books)`.
        let SourceSchemaHoverFact::Resource(res) =
            schema_fact_from(&snapshot, &index, &file, offset)
        else {
            panic!("resource annotation must resolve to the resource schema");
        };
        assert!(res.docs.iter().any(|line| line == "Books saved by id."));
        assert!(resource_has_field(&res, "title", "string"));
    }
}

#[test]
fn hover_over_evolve_transform_type_annotation_shows_resource_hover() {
    let source = "\
module a

;; Books saved by id.
resource Book
    required title: string
    required slug: string

store ^books(id: int): Book

evolve
    transform Book.title
        const draft: Book = Book(title: old.slug, slug: old.slug)
        return draft.title
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);

    let offset = offset_of(source, "draft: Book") + "draft: ".len();
    let hover = hover_with_index(&snapshot, &index, &file, offset).expect("resource type hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    let value = markup.value;

    assert!(
        value.starts_with("```marrow\nresource Book\n```"),
        "resource annotation hover should lead with resource signature: {value}"
    );
    let SourceSchemaHoverFact::Resource(res) = schema_fact_from(&snapshot, &index, &file, offset)
    else {
        panic!("evolve transform annotation must resolve to the resource schema");
    };
    assert!(res.docs.iter().any(|line| line == "Books saved by id."));
    assert!(resource_has_field(&res, "title", "string"));
}

#[test]
fn hover_over_same_named_resource_type_annotation_uses_current_module_schema() {
    let first_source = "\
module shelf::first

;; First module books.
resource Book
    required title: string
";
    let second_source = "\
module shelf::second

;; Second module books.
resource Book
    required subtitle: string

pub fn echo(book: Book): Book
    return book
";
    let (snapshot, file) = analyze_files(
        &[
            ("shelf/first.mw", first_source),
            ("shelf/second.mw", second_source),
        ],
        "shelf/second.mw",
    );
    let index = index_for(&snapshot);
    let offset = offset_of(second_source, "book: Book") + "book: ".len();
    let hover = hover_with_index(&snapshot, &index, &file, offset).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    let value = markup.value;

    assert!(
        value.starts_with("```marrow\nresource Book\n```"),
        "resource annotation hover should use the current module schema: {value}"
    );
    // The annotation resolves to the current module's resource schema — its docs and members, not
    // the same-named resource in the sibling module (and a resource fact carries no store keys).
    let SourceSchemaHoverFact::Resource(res) = schema_fact_from(&snapshot, &index, &file, offset)
    else {
        panic!("expected the current module's resource schema");
    };
    assert!(res.docs.iter().any(|line| line == "Second module books."));
    assert!(resource_has_field(&res, "subtitle", "string"));
    assert!(!res.docs.iter().any(|line| line == "First module books."));
    assert!(!resource_has_field(&res, "id", "int"));
}

#[test]
fn hover_over_wrapped_qualified_resource_type_annotation_shows_resource_hover() {
    let state_source = "\
module shelf::state

;; Books from state.
resource Book
    required title: string
";
    let app_source = "\
module shelf::app

use shelf::state

;; App-local books.
resource Book
    required subtitle: string

pub fn books(items: sequence[state::Book])
    return
";
    let (snapshot, file) = analyze_files(
        &[
            ("shelf/state.mw", state_source),
            ("shelf/app.mw", app_source),
        ],
        "shelf/app.mw",
    );
    let index = index_for(&snapshot);
    let annotation = offset_of(app_source, "state::Book");
    let qualifier_hover = hover_with_index(&snapshot, &index, &file, annotation + 1);
    if let Some(Hover {
        contents: HoverContents::Markup(markup),
        ..
    }) = qualifier_hover
    {
        assert!(
            !markup.value.starts_with("```marrow\nresource Book\n```"),
            "module qualifier should not pretend to be the resource: {}",
            markup.value
        );
    }

    let leaf = annotation + "state::".len();
    let hover = hover_with_index(&snapshot, &index, &file, leaf).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    let value = markup.value;

    assert!(
        value.starts_with("```marrow\nresource Book\n```"),
        "wrapped resource annotation hover should use the qualified module schema: {value}"
    );
    // The wrapped `state::Book` resolves to the foreign module's resource schema, not the app-local
    // same-named resource.
    let SourceSchemaHoverFact::Resource(res) = schema_fact_from(&snapshot, &index, &file, leaf)
    else {
        panic!("expected the foreign module's resource schema");
    };
    assert!(res.docs.iter().any(|line| line == "Books from state."));
    assert!(resource_has_field(&res, "title", "string"));
    assert!(!res.docs.iter().any(|line| line == "App-local books."));
    assert!(!resource_has_field(&res, "subtitle", "string"));
}

#[test]
fn hover_over_same_named_resource_declaration_uses_that_module_schema() {
    let first_source = "\
module shelf::first

;; First module books.
resource Book
    required title: string
";
    let second_source = "\
module shelf::second

;; Second module books.
resource Book
    required subtitle: string
";
    let (snapshot, file) = analyze_files(
        &[
            ("shelf/first.mw", first_source),
            ("shelf/second.mw", second_source),
        ],
        "shelf/second.mw",
    );
    let index = index_for(&snapshot);
    let offset = offset_of(second_source, "Book");
    let value = hover_value(&snapshot, &index, &file, second_source, "Book");
    assert!(
        value.starts_with("```marrow\nresource Book\n```"),
        "resource hover should use the declaring module schema: {value}"
    );
    let SourceSchemaHoverFact::Resource(res) = schema_fact_from(&snapshot, &index, &file, offset)
    else {
        panic!("expected the declaring module's resource schema");
    };
    assert!(res.docs.iter().any(|line| line == "Second module books."));
    assert!(resource_has_field(&res, "subtitle", "string"));
    assert!(!res.docs.iter().any(|line| line == "First module books."));
    assert!(!resource_has_field(&res, "id", "int"));
}

#[test]
fn hover_over_saved_root_declaration_shows_store_hover() {
    let source = "\
module a

resource Book
    ;; Display title.
    required title: string

;; Books saved by id.
store ^books(id: int): Book

pub fn f(): string
    return ^books(1).title
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let declaration_caret = offset_of(source, "^books");
    let declaration_root = declaration_caret + 1;

    for offset in [declaration_caret, declaration_root] {
        let hover = hover_with_index(&snapshot, &index, &file, offset).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        assert!(
            markup
                .value
                .starts_with("```marrow\nstore ^books(id: int): Book\n```"),
            "saved root hover should show the store signature: {}",
            markup.value
        );
        // The store docs are a typed payload; the "Id(^books)" identity type is rendered from the
        // store root's identity keys, which a keyed store carries.
        let store = store_root_fact_from(&snapshot, &index, &file, offset);
        assert!(
            store
                .store_docs
                .iter()
                .any(|line| line == "Books saved by id.")
        );
        assert!(!store.identity_keys.is_empty());
    }

    let use_caret = source.rfind("^books").unwrap();
    let use_root = use_caret + 1;
    for offset in [use_caret, use_root] {
        if let Some(hover) = hover_with_index(&snapshot, &index, &file, offset) {
            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markup");
            };
            assert!(
                !markup
                    .value
                    .starts_with("```marrow\nstore ^books(id: int): Book\n```"),
                "saved root use should not use store hover: {}",
                markup.value
            );
        }
    }

    let title = source.rfind(").title").unwrap() + ").".len() + 1;
    let hover = hover_with_index(&snapshot, &index, &file, title).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    let value = markup.value;

    assert!(
        value.starts_with("```marrow\nrequired title: string\n```"),
        "saved field hover should stay on the member: {value}"
    );
    assert!(
        saved_place_docs(&saved_place_fact_from(&snapshot, &index, &file, title))
            .iter()
            .any(|line| line == "Display title.")
    );
}

#[test]
fn hover_over_saved_root_declaration_works_when_resource_name_matches_root() {
    let source = "\
module a

resource books
    required title: string

;; Books saved by id.
store ^books(id: int): books
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "^books") + 1;
    let hover = hover_with_index(&snapshot, &index, &file, offset).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    let value = markup.value;

    assert!(
        value.starts_with("```marrow\nstore ^books(id: int): books\n```"),
        "saved root hover should use the store signature: {value}"
    );
    assert!(
        store_root_fact_from(&snapshot, &index, &file, offset)
            .store_docs
            .iter()
            .any(|line| line == "Books saved by id.")
    );
}

#[test]
fn resource_member_summary_is_bounded() {
    let source = "\
module a

resource Book
    one: string
    two: string
    three: string
    four: string
    five: string
    six: string
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    // The member-summary bound (which members render and the "... N more" tail) is a pure rendering
    // decision over the full typed member list, so the reviewed golden is the oracle.
    let value = hover_value(&snapshot, &index, &file, source, "Book");
    assert_hover_golden("resource_member_summary_bounded", &value);
}

#[test]
fn resource_member_summary_shows_nested_members_bounded() {
    let source = "\
module a

resource Book
    details
        required subtitle: string
        pages: int
    editions(number: int)
        isbn: string
        format: string
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    // The nested/keyed member paths and the bound are a pure rendering of the typed member tree, so
    // the reviewed golden pins the flattened summary.
    let value = hover_value(&snapshot, &index, &file, source, "Book");
    assert_hover_golden("resource_member_summary_nested_bounded", &value);
}

#[test]
fn hover_over_a_saved_root_use_stays_type_only() {
    let source = "\
module a

resource Book
    required title: string

;; Books saved by id.
store ^books(id: int): Book

pub fn clear()
    delete ^books
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "delete ^books") + "delete ".len();
    if let Some(hover) = hover_with_index(&snapshot, &index, &file, offset) {
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        assert!(
            !markup
                .value
                .starts_with("```marrow\nstore ^books(id: int): Book\n```"),
            "saved root use should not use store hover: {}",
            markup.value
        );
    }
}

#[test]
fn hover_over_saved_field_use_shows_member_signature_and_docs() {
    let source = "\
module a

resource Book
    ;; The displayed title.
    required title: string

store ^books(id: int): Book

pub fn f(): string
    return ^books(1).title
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = source.rfind(").title").unwrap() + ").".len() + 1;
    let hover = hover_with_index(&snapshot, &index, &file, offset).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    let value = markup.value;

    assert!(
        value.starts_with("```marrow\nrequired title: string\n```"),
        "field member signature should lead the hover: {value}"
    );
    assert!(
        saved_place_docs(&saved_place_fact_from(&snapshot, &index, &file, offset))
            .iter()
            .any(|line| line == "The displayed title.")
    );
}

#[test]
fn hover_over_resource_field_declaration_name_shows_member_signature() {
    let source = "\
module a

resource Book
    ;; The displayed title.
    required title: string

store ^books(id: int): Book
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "required title") + "required ".len() + 1;
    let hover = hover_with_index(&snapshot, &index, &file, offset).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    let value = markup.value;

    assert!(
        value.starts_with("```marrow\nrequired title: string\n```"),
        "field declaration hover should lead with the member signature: {value}"
    );
    assert!(
        saved_place_docs(&saved_place_fact_from(&snapshot, &index, &file, offset))
            .iter()
            .any(|line| line == "The displayed title.")
    );
}

#[test]
fn hover_over_a_saved_root_declaration_shows_store_signature() {
    let source = "\
module a

resource Book
    required title: string

;; Books saved by id.
store ^books(id: int): Book
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "^books") + 1;
    let hover = hover_with_index(&snapshot, &index, &file, offset).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    let value = markup.value;

    assert!(
        value.starts_with("```marrow\nstore ^books(id: int): Book\n```"),
        "declared saved root should show its store even without live data: {value}"
    );
    assert!(
        store_root_fact_from(&snapshot, &index, &file, offset)
            .store_docs
            .iter()
            .any(|line| line == "Books saved by id.")
    );
    // Hover stays analysis-only: no live-data advisory section is rendered. There is no typed fact
    // for the absence of an advisory, so this remains a render-level guard.
    assert!(
        !value.contains("**debug/admin live data (advisory)**"),
        "hover should stay analysis-only: {value}"
    );
}

#[test]
fn hover_over_keyless_store_root_says_no_generated_identity_type() {
    let source = "\
module a

resource Settings
    theme: string

;; Singleton settings.
store ^settings: Settings
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "^settings");
    let value = hover_value(&snapshot, &index, &file, source, "^settings");
    assert!(
        value.starts_with("```marrow\nstore ^settings: Settings\n```"),
        "keyless store hover should lead with the store signature: {value}"
    );
    // "no generated identity type" renders precisely when the store root has no identity keys.
    assert!(
        store_root_fact_from(&snapshot, &index, &file, offset)
            .identity_keys
            .is_empty()
    );
}

#[test]
fn hover_over_an_enum_declaration_name_shows_docs_and_members() {
    let source = "\
module a

;; Lifecycle state.
pub enum Status
    open
    closed
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "Status");
    let value = hover_value(&snapshot, &index, &file, source, "Status");
    assert!(
        value.starts_with("```marrow\nenum Status\n```"),
        "enum signature should lead the hover: {value}"
    );
    // The enum fact carries docs and a typed member list (no source-order ordinal), which the
    // summary renders.
    let SourceSchemaHoverFact::Enum(en) = schema_fact_from(&snapshot, &index, &file, offset) else {
        panic!("expected an enum schema hover fact");
    };
    assert!(en.docs.iter().any(|line| line == "Lifecycle state."));
    let members: Vec<String> = en.members.iter().map(|m| m.path.join("::")).collect();
    assert!(members.iter().any(|name| name == "open"));
    assert!(members.iter().any(|name| name == "closed"));
}

#[test]
fn hover_over_the_enum_keyword_does_not_show_the_enum() {
    let source = "\
module a

;; Lifecycle state.
pub enum Status
    open
    closed
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "enum Status");
    let hover = hover_with_index(&snapshot, &index, &file, offset);
    if let Some(Hover {
        contents: HoverContents::Markup(markup),
        ..
    }) = hover
    {
        assert!(
            !markup.value.starts_with("```marrow\nenum Status\n```"),
            "enum keyword should not inherit the declaration-name hover: {}",
            markup.value
        );
    }
}

#[test]
fn hover_over_an_enum_type_annotation_shows_docs_and_members() {
    let source = "\
module a

;; Lifecycle state.
enum Status
    open
    closed

pub fn set(status: Status)
    return
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "status: Status") + "status: ".len();
    assert_status_enum_fact(&snapshot, &index, &file, offset);
}

#[test]
fn hover_over_parameter_name_matching_enum_does_not_show_enum_hover() {
    let source = "\
module a

;; Lifecycle state.
enum Status
    open
    closed

pub fn set(Status: int)
    return
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "Status: int");
    let hover = hover_with_index(&snapshot, &index, &file, offset);
    if let Some(Hover {
        contents: HoverContents::Markup(markup),
        ..
    }) = hover
    {
        assert!(
            !markup.value.starts_with("```marrow\nenum Status\n```"),
            "parameter name should not be treated as an enum type: {}",
            markup.value
        );
    }
}

#[test]
fn hover_over_qualified_foreign_enum_type_annotation_uses_catalog_fact() {
    let state_source = "\
module shelf::state

;; Lifecycle state.
pub enum Status
    open
    closed
";
    let app_source = "\
module shelf::app

use shelf::state

pub fn set(status: state::Status)
    return
";
    let (snapshot, file) = analyze_files(
        &[
            ("shelf/state.mw", state_source),
            ("shelf/app.mw", app_source),
        ],
        "shelf/app.mw",
    );
    let index = index_for(&snapshot);
    let offset = offset_of(app_source, "state::Status") + "state::".len();
    assert_status_enum_fact(&snapshot, &index, &file, offset);
}

#[test]
fn hover_over_bare_foreign_enum_type_annotation_uses_catalog_fact() {
    let other_source = "\
module shelf::other

pub fn noop()
    return
";
    let state_source = "\
module shelf::state

;; Lifecycle state.
pub enum Status
    open
    closed
";
    let app_source = "\
module shelf::app

pub fn set(status: Status)
    return
";
    let (snapshot, file) = analyze_files(
        &[
            ("shelf/other.mw", other_source),
            ("shelf/state.mw", state_source),
            ("shelf/app.mw", app_source),
        ],
        "shelf/app.mw",
    );
    let index = index_for(&snapshot);
    let offset = offset_of(app_source, "status: Status") + "status: ".len();
    assert_status_enum_fact(&snapshot, &index, &file, offset);
}

#[test]
fn hover_over_ambiguous_bare_foreign_enum_type_annotation_does_not_pick_an_enum() {
    let first_source = "\
module shelf::first

;; First lifecycle state.
pub enum Status
    open
";
    let second_source = "\
module shelf::second

;; Second lifecycle state.
pub enum Status
    closed
";
    let app_source = "\
module shelf::app

pub fn set(status: Status)
    return
";
    let (snapshot, file) = analyze_files(
        &[
            ("shelf/first.mw", first_source),
            ("shelf/second.mw", second_source),
            ("shelf/app.mw", app_source),
        ],
        "shelf/app.mw",
    );
    let index = index_for(&snapshot);
    let offset = offset_of(app_source, "status: Status") + "status: ".len();
    // Two equally-named foreign enums are ambiguous, so the annotation must not resolve to either
    // enum schema fact.
    assert!(
        !matches!(
            source_hover_fact_at(&snapshot, &index, &file, offset),
            Some(SourceHoverFact::Schema(SourceSchemaHoverFact::Enum(_)))
        ),
        "ambiguous bare enum annotation must not pick a foreign enum"
    );
}

#[test]
fn hover_over_private_foreign_enum_type_annotation_does_not_show_enum_hover() {
    let state_source = "\
module shelf::state

;; Private lifecycle state.
enum Status
    open
    closed
";
    let app_source = "\
module shelf::app

use shelf::state

pub fn set(status: state::Status)
    return
";
    let (snapshot, file) = analyze_files(
        &[
            ("shelf/state.mw", state_source),
            ("shelf/app.mw", app_source),
        ],
        "shelf/app.mw",
    );
    let index = index_for(&snapshot);
    let offset = offset_of(app_source, "state::Status") + "state::".len();
    // A private foreign enum is not visible, so the annotation must not resolve to its enum schema.
    assert!(
        !matches!(
            source_hover_fact_at(&snapshot, &index, &file, offset),
            Some(SourceHoverFact::Schema(SourceSchemaHoverFact::Enum(_)))
        ),
        "private foreign enum annotation must not show the enum hover"
    );
}

#[test]
fn hover_over_an_enum_member_value_shows_path_status_and_docs() {
    let source = "\
module a

enum Status
    category active
        ;; Open for edits.
        open
    closed

pub fn current(): Status
    return Status::active::open
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = source.rfind("Status::active::open").unwrap() + "Status::active::".len();
    let hover = hover_with_index(&snapshot, &index, &file, offset).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    let value = markup.value;

    assert!(
        value.starts_with("```marrow\nStatus::active::open\n```"),
        "enum member hover should lead with the full member path: {value}"
    );
    // The enum name, member path, selectable status, and docs are typed payloads (no source-order
    // ordinal exists on the fact).
    let SourceSchemaHoverFact::EnumMember(member) =
        schema_fact_from(&snapshot, &index, &file, offset)
    else {
        panic!("expected an enum-member schema hover fact");
    };
    assert_eq!(member.enum_name, "Status");
    assert_eq!(member.path, ["active", "open"]);
    assert_eq!(member.status, SourceEnumMemberStatus::Selectable);
    assert!(member.docs.iter().any(|line| line == "Open for edits."));
}

#[test]
fn hover_over_a_function_call_stays_rich_after_resource_enum_hovers() {
    let source = "\
module a

;; Adds two numbers.
pub fn add(x: int, y: int): int
    return x + y

pub fn caller(): int
    return add(1, 2)
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "add(1, 2)");
    let value = hover_value(&snapshot, &index, &file, source, "add(1, 2)");
    assert!(
        value.starts_with("```marrow\nfn add(x: int, y: int): int\n```"),
        "function hovers should keep leading with the signature: {value}"
    );
    assert!(
        callable_docs(&callable_fact_from(&snapshot, &index, &file, offset))
            .iter()
            .any(|line| line == "Adds two numbers.")
    );
}

#[test]
fn hover_over_a_qualified_function_call_only_uses_the_leaf_as_the_function() {
    let math_source = "\
module shelf::math

;; Adds two numbers.
pub fn add(x: int, y: int): int
    return x + y
";
    let app_source = "\
module shelf::app

use shelf::math

pub fn caller(): int
    return math::add(1, 2)
";
    let (snapshot, file) = analyze_files(
        &[("shelf/math.mw", math_source), ("shelf/app.mw", app_source)],
        "shelf/app.mw",
    );
    let index = index_for(&snapshot);
    let call = app_source.rfind("math::add(1, 2)").unwrap();
    // The module qualifier must not resolve to the callee function (which would inherit its docs).
    assert!(
        !matches!(
            source_hover_fact_at(&snapshot, &index, &file, call + 1),
            Some(SourceHoverFact::Callable(
                SourceCallableHoverFact::Function(_)
            ))
        ),
        "module qualifier hover must not pretend to be the function"
    );

    let leaf_offset = call + "math::".len() + 1;
    let leaf_hover = hover(&snapshot, &file, leaf_offset).expect("a hover at the callee leaf");
    let HoverContents::Markup(markup) = leaf_hover.contents else {
        panic!("expected markup contents");
    };
    assert!(
        markup
            .value
            .starts_with("```marrow\nfn add(x: int, y: int): int\n```"),
        "function leaf should show the signature: {}",
        markup.value
    );
}

#[test]
fn hover_off_any_expression_is_none() {
    let source = "module a\n\npub fn f(): int\n    return 1\n";
    let (snapshot, file) = analyze(source);
    // Offset 0 sits on the `module` keyword, inside no function body.
    assert!(hover(&snapshot, &file, 0).is_none());
}

#[test]
fn hover_for_an_unknown_file_is_none() {
    let source = "module a\n\npub fn f(): int\n    return 1\n";
    let (snapshot, _) = analyze(source);
    let stray = std::path::Path::new("/nowhere/b.mw");
    assert!(hover(&snapshot, stray, 0).is_none());
}

#[test]
fn hover_without_docs_is_type_only() {
    let hover = render::hover(
        &CheckedProgram::default(),
        SourceHoverFact::Type(marrow_check::tooling::SourceTypeHoverFact {
            ty: marrow_check::MarrowType::Primitive(marrow_check::ScalarType::Str),
            docs: Vec::new(),
        }),
    );
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    assert_eq!(markup.value, "```marrow\nstring\n```");
}

#[test]
fn explicit_dynamic_local_hover_renders_unknown() {
    let source = "\
module a

pub fn f(value: unknown)
    const copy = value
    print(copy)
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = source.rfind("copy").unwrap();

    let value = hover_value_at(&snapshot, &index, &file, offset).expect("dynamic local hover");
    assert_eq!(value, "```marrow\nunknown\n```");
    assert!(matches!(
        source_hover_fact_at(&snapshot, &index, &file, offset),
        Some(SourceHoverFact::Type(SourceTypeHoverFact {
            ty: marrow_check::MarrowType::Dynamic,
            ..
        }))
    ));
}

#[test]
fn no_value_call_closing_edge_has_no_hover() {
    let source = "\
module a

fn finish()
    return

pub fn caller()
    finish()
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = source.rfind("finish()").unwrap() + "finish(".len();

    assert!(hover_value_at(&snapshot, &index, &file, offset).is_none());
}

#[test]
fn diagnosed_unresolved_name_has_no_type_hover() {
    let source = "module a\n\npub fn f(): int\n    return missing\n";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = source.rfind("missing").unwrap();

    assert!(hover_value_at(&snapshot, &index, &file, offset).is_none());
}

/// Build the binding index for a snapshot so a docs test can resolve a symbol.
fn index_for(snapshot: &AnalysisSnapshot) -> BindingIndex {
    marrow_check::build_binding_index(snapshot)
}

#[test]
fn hover_over_an_optional_returning_call_shows_the_optional_return_type() {
    let source = "\
module a

fn findSub(id: int): string?
    return absent

pub fn caller(): string
    return findSub(1) ?? \"missing\"
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let value = hover_value(&snapshot, &index, &file, source, "findSub(1)");

    assert!(
        value.starts_with("```marrow\nfn findSub(id: int): string?\n```"),
        "an optional return type should render with the `?` suffix: {value}"
    );
}

#[test]
fn hover_over_an_optional_binding_shows_the_optional_type_and_narrows_when_resolved() {
    let source = "\
module a

fn findSub(id: int): string?
    return absent

pub fn caller()
    const maybe = findSub(1)
    std::assert::isAbsent(maybe)
    if const found = findSub(1)
        print(found)
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);

    // The inferred binding of a `T?` call carries the optional type; the `?` must
    // survive into the hover rather than being dropped to the bare element type.
    let maybe_use = offset_of(source, "isAbsent(maybe)") + "isAbsent(".len();
    let maybe =
        hover_value_at(&snapshot, &index, &file, maybe_use).expect("optional binding hover");
    assert_eq!(maybe, "```marrow\nstring?\n```");

    // `if const` resolves the optional, so the narrowed binding is a plain `string`.
    let found_use = offset_of(source, "print(found)") + "print(".len();
    let found =
        hover_value_at(&snapshot, &index, &file, found_use).expect("narrowed binding hover");
    assert_eq!(found, "```marrow\nstring\n```");
}

#[test]
fn symbol_docs_for_a_documented_function_returns_its_description() {
    let source = "\
module a

;; Adds two numbers.
pub fn add(x: int, y: int): int
    return x + y

pub fn caller(): int
    return add(1, 2)
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    // Hover over the call `add(1, 2)`.
    let offset = source.rfind("add(1, 2)").unwrap();
    let docs = symbol_docs(&snapshot, &index, &file, offset).expect("function docs");
    assert_eq!(docs, "Adds two numbers.");
}

#[test]
fn hover_over_a_documented_module_const_use_shows_type_and_description() {
    let source = "\
module a

;; Maximum count.
const LIMIT: int = 10

pub fn caller(): int
    return LIMIT
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = source.rfind("LIMIT").unwrap();
    let value = hover_value_at(&snapshot, &index, &file, offset).expect("module const use hover");
    assert!(
        value.starts_with("```marrow\nint\n```"),
        "the type should lead the hover: {value}"
    );
    // A const use resolves to the type hover fact, which carries the const's docs.
    assert!(
        type_fact_from(&snapshot, &index, &file, offset)
            .docs
            .iter()
            .any(|line| line == "Maximum count.")
    );
}

#[test]
fn hover_over_a_documented_module_const_declaration_shows_type_and_description() {
    let source = "\
module a

;; Maximum count.
const LIMIT: int = 10

pub fn caller(): int
    return LIMIT
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = offset_of(source, "LIMIT: int") + 1;
    let value =
        hover_value_at(&snapshot, &index, &file, offset).expect("module const declaration hover");

    assert!(
        value.starts_with("```marrow\nconst LIMIT: int\n```"),
        "the module const signature should lead the hover: {value}"
    );
    assert!(
        callable_docs(&callable_fact_from(&snapshot, &index, &file, offset))
            .iter()
            .any(|line| line == "Maximum count.")
    );
}

#[test]
fn symbol_docs_for_a_resource_name_returns_its_description() {
    let source = "\
module a

;; A book on a shelf.
resource Book
    required title: string

store ^books(id: int): Book

pub fn f(): Book
    return Book(title: \"x\")
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    // Hover over the constructor use `Book(...)`.
    let offset = source.rfind("Book(title:").unwrap();
    let docs = symbol_docs(&snapshot, &index, &file, offset).expect("resource docs");
    assert_eq!(docs, "A book on a shelf.");
}

#[test]
fn symbol_docs_for_a_saved_field_returns_its_description() {
    let source = "\
module a

resource Book
    ;; The book's title.
    required title: string

store ^books(id: int): Book

pub fn f(): string
    return ^books(1).title ?? \"\"
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    // Hover over `title` in `^books(1).title`.
    let offset = source.rfind(").title").unwrap() + ").".len() + 1;
    let docs = symbol_docs(&snapshot, &index, &file, offset).expect("field docs");
    assert_eq!(docs, "The book's title.");
}

#[test]
fn symbol_docs_for_a_documented_enum_returns_its_description() {
    let source = "\
module a

;; Lifecycle state.
enum Status
    open
    closed
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = source.find("Status").unwrap() + 1;
    let docs = symbol_docs(&snapshot, &index, &file, offset).expect("enum docs");
    assert_eq!(docs, "Lifecycle state.");
}

#[test]
fn symbol_docs_for_a_documented_enum_member_returns_its_description() {
    let source = "\
module a

enum Status
    ;; Open for edits.
    open
    closed

pub fn current(): Status
    return Status::open
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = source.rfind("Status::open").unwrap() + "Status::".len() + 1;
    let docs = symbol_docs(&snapshot, &index, &file, offset).expect("enum member docs");
    assert_eq!(docs, "Open for edits.");
}

#[test]
fn symbol_docs_for_an_undocumented_symbol_is_none() {
    let source = "\
module a

pub fn add(x: int, y: int): int
    return x + y

pub fn caller(): int
    return add(1, 2)
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let offset = source.rfind("add(1, 2)").unwrap();
    assert!(
        symbol_docs(&snapshot, &index, &file, offset).is_none(),
        "an undocumented function has no description"
    );
}

#[test]
fn symbol_docs_for_a_local_or_param_is_none() {
    let source = "module a\n\npub fn f(n: int): int\n    return n\n";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    // Hover over the parameter use `n` in `return n`.
    let offset = offset_of(source, "return n") + "return ".len();
    assert!(
        symbol_docs(&snapshot, &index, &file, offset).is_none(),
        "a parameter carries no `;;` documentation"
    );
}
