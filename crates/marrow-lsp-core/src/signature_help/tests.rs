use super::*;
use lsp_types::{Documentation, ParameterLabel};
use marrow_check::{AnalysisSnapshot, ProjectSources, analyze_project};
use marrow_project::parse_config;
use marrow_syntax::lex_source;

fn project_snapshot() -> (AnalysisSnapshot, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let pkg = root.join("src/shelf");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("books.mw"),
        "\
module shelf::books

enum Status
    active

;; Books stored in the public shelf.
resource Book
    ;; Title shown to readers.
    required title: string
    ;; Page count from the catalog.
    pages: int
    tags(pos: int): string

store ^books(id: int, edition: string): Book

resource Settings
    enabled: bool

store ^settings: Settings

resource Badge
    required value: int

resource Review
    status: Status

;; Resolves the display title for a book.
pub fn titleOf(
    ;; Book identity to resolve.
    id: Id(^books),
    ;; Title to use when the book is missing.
    fallback: string,
): string
    return fallback
",
    )
    .unwrap();
    let app = pkg.join("app.mw");
    std::fs::write(
        &app,
        "\
module shelf::app

use shelf::books

;; Adds two integers.
fn add(
    ;; Left addend.
    left: int,
    ;; Right addend.
    right: int,
): int
    return left + right

fn localBooks(id: int): int
    return id

fn id(value: int): int
    return value

fn parse(text: string, value: int, count: int): bool
    return true

fn title(value: string): string
    return value

pub fn run(): int
    return 1
",
    )
    .unwrap();
    let config =
        parse_config(r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#).unwrap();
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None, None).unwrap();
    (snapshot, app)
}

fn project() -> (CheckedProgram, std::path::PathBuf) {
    let (snapshot, app) = project_snapshot();
    (snapshot.program, app)
}

fn help_at(program: &CheckedProgram, file: &Path, source: &str) -> Option<SignatureHelp> {
    let offset = source.find('|').expect("a cursor marker `|`");
    let source = source.replacen('|', "", 1);
    let lexed = lex_source(&source);
    signature_help(program, None, file, &source, &lexed, offset)
}

fn help_with_docs_at(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    source: &str,
) -> Option<SignatureHelp> {
    let offset = source.find('|').expect("a cursor marker `|`");
    let source = source.replacen('|', "", 1);
    let lexed = lex_source(&source);
    signature_help(
        &snapshot.program,
        Some(snapshot),
        file,
        &source,
        &lexed,
        offset,
    )
}

fn help_in_single_file_at(source: &str) -> Option<SignatureHelp> {
    let offset = source.find('|').expect("a cursor marker `|`");
    let analysis_source = source.replacen('|', "\"b\")", 1);
    let active_source = source.replacen('|', "", 1);

    let snapshot = single_file_snapshot(&analysis_source);
    let file = snapshot
        .files
        .iter()
        .find(|file| file.path.ends_with("app.mw"))
        .expect("the analyzed app source")
        .path
        .clone();
    let lexed = lex_source(&active_source);
    signature_help(
        &snapshot.program,
        Some(&snapshot),
        &file,
        &active_source,
        &lexed,
        offset,
    )
}

fn single_file_snapshot(source: &str) -> AnalysisSnapshot {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("app.mw");
    std::fs::write(&file, source).unwrap();

    let config =
        parse_config(r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#).unwrap();
    analyze_project(root, &config, &ProjectSources::new(), None, None).unwrap()
}

fn signature_label(help: &SignatureHelp) -> &str {
    &help.signatures[0].label
}

fn signature_documentation(help: &SignatureHelp) -> &str {
    documentation_value(
        help.signatures[0]
            .documentation
            .as_ref()
            .expect("signature documentation"),
    )
}

fn parameter_labels(help: &SignatureHelp) -> Vec<String> {
    help.signatures[0]
        .parameters
        .as_ref()
        .expect("parameter information")
        .iter()
        .map(|param| match &param.label {
            ParameterLabel::Simple(label) => label.clone(),
            ParameterLabel::LabelOffsets(_) => panic!("expected simple labels"),
        })
        .collect()
}

fn parameter_documentation(help: &SignatureHelp, index: usize) -> &str {
    let parameter = &help.signatures[0]
        .parameters
        .as_ref()
        .expect("parameter information")[index];
    documentation_value(
        parameter
            .documentation
            .as_ref()
            .expect("parameter documentation"),
    )
}

fn documentation_value(documentation: &Documentation) -> &str {
    match documentation {
        Documentation::String(value) => value,
        Documentation::MarkupContent(content) => &content.value,
    }
}

#[test]
fn documented_user_function_signature_help_includes_signature_and_parameter_docs() {
    let (snapshot, file) = project_snapshot();
    let help = help_with_docs_at(
        &snapshot,
        &file,
        "module shelf::app\n\npub fn run(): int\n    return add(|\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "add(left: int, right: int): int");
    assert_eq!(signature_documentation(&help), "Adds two integers.");
    assert_eq!(parameter_documentation(&help, 0), "Left addend.");
    assert_eq!(parameter_documentation(&help, 1), "Right addend.");
}

#[test]
fn qualified_imported_function_signature_help_reads_docs_from_defining_module() {
    let (snapshot, file) = project_snapshot();
    let help = help_with_docs_at(
            &snapshot,
            &file,
            "module shelf::app\n\nuse shelf::books\n\npub fn run(): string\n    return books::titleOf(|\n",
        )
        .expect("signature help");

    assert_eq!(
        signature_label(&help),
        "titleOf(id: Id(^books), fallback: string): string"
    );
    assert_eq!(
        signature_documentation(&help),
        "Resolves the display title for a book."
    );
    assert_eq!(
        parameter_documentation(&help, 0),
        "Book identity to resolve."
    );
    assert_eq!(
        parameter_documentation(&help, 1),
        "Title to use when the book is missing."
    );
}

#[test]
fn resource_constructor_signature_help_includes_resource_and_field_docs() {
    let (snapshot, file) = project_snapshot();
    let help = help_with_docs_at(
            &snapshot,
            &file,
            "module shelf::app\n\nuse shelf::books\n\npub fn run(): Book\n    return books::Book(pages: |\n",
        )
        .expect("signature help");

    assert_eq!(
        signature_label(&help),
        "Book(title: string, pages: int): Book"
    );
    assert_eq!(
        signature_documentation(&help),
        "Books stored in the public shelf."
    );
    assert_eq!(parameter_documentation(&help, 0), "Title shown to readers.");
    assert_eq!(
        parameter_documentation(&help, 1),
        "Page count from the catalog."
    );
}

#[test]
fn bare_builtin_signature_help_includes_canonical_description() {
    let (snapshot, file) = project_snapshot();
    let help = help_with_docs_at(
        &snapshot,
        &file,
        "module shelf::app\n\npub fn run(): int\n    return count(|\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "count(collection): int");
    assert_eq!(parameter_labels(&help), vec!["collection".to_string()]);
    assert_eq!(
        signature_documentation(&help),
        "Returns child count for a saved path, 1 for a scalar, or 0 when absent."
    );
}

#[test]
fn user_function_call_marks_first_parameter_active() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): int\n    return add(|\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "add(left: int, right: int): int");
    assert_eq!(
        parameter_labels(&help),
        vec!["left: int".to_string(), "right: int".to_string()]
    );
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn user_function_call_marks_second_parameter_active() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): int\n    return add(1, |\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "add(left: int, right: int): int");
    assert_eq!(help.active_parameter, Some(1));
}

#[test]
fn qualified_imported_function_call_resolves_through_checker() {
    let (program, file) = project();
    let help = help_at(
            &program,
            &file,
            "module shelf::app\n\nuse shelf::books\n\npub fn run(): string\n    return books::titleOf(|\n",
        )
        .expect("signature help");

    assert_eq!(
        signature_label(&help),
        "titleOf(id: Id(^books), fallback: string): string"
    );
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn user_function_signature_includes_multiple_parameters() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): bool\n    return parse(\"12\", |\n",
    )
    .expect("signature help");

    assert_eq!(
        signature_label(&help),
        "parse(text: string, value: int, count: int): bool"
    );
    assert_eq!(
        parameter_labels(&help),
        vec![
            "text: string".to_string(),
            "value: int".to_string(),
            "count: int".to_string(),
        ]
    );
    assert_eq!(help.active_parameter, Some(1));
}

#[test]
fn resource_constructor_uses_named_plain_fields() {
    let (program, file) = project();
    let help = help_at(
            &program,
            &file,
            "module shelf::app\n\nuse shelf::books\n\npub fn run(): Book\n    return books::Book(pages: |\n",
        )
        .expect("signature help");

    assert_eq!(
        signature_label(&help),
        "Book(title: string, pages: int): Book"
    );
    assert_eq!(
        parameter_labels(&help),
        vec!["title: string".to_string(), "pages: int".to_string()]
    );
    assert_eq!(help.active_parameter, Some(1));
}

#[test]
fn imported_module_resource_constructor_keeps_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn run(): books::Badge\n    return books::Badge(|\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "Badge(value: int): Badge");
    assert_eq!(parameter_labels(&help), vec!["value: int".to_string()]);
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn fully_qualified_resource_constructor_keeps_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): shelf::books::Badge\n    return shelf::books::Badge(|\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "Badge(value: int): Badge");
    assert_eq!(parameter_labels(&help), vec!["value: int".to_string()]);
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn resource_constructor_signature_help_uses_checked_field_types() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn run(): books::Review\n    return books::Review(|\n",
    )
    .expect("signature help");

    assert_eq!(
        signature_label(&help),
        "Review(status: shelf::books::Status): Review"
    );
    assert_eq!(
        parameter_labels(&help),
        vec!["status: shelf::books::Status".to_string()]
    );
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn std_operation_uses_canonical_table_signature() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): bool\n    return std::text::contains(\"abc\", |\n",
    )
    .expect("signature help");

    assert_eq!(
        signature_label(&help),
        "std::text::contains(string, string): bool"
    );
    assert_eq!(
        parameter_labels(&help),
        vec!["string".to_string(), "string".to_string()]
    );
    assert_eq!(help.active_parameter, Some(1));
}

#[test]
fn signature_help_renders_optional_parameter_and_return_types() {
    let help = help_in_single_file_at(
        "module app\n\
        fn label(tag: string?): string?\n    \
        return tag\n\
        fn run(): string?\n    \
        return label(|\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "label(tag: string?): string?");
    assert_eq!(parameter_labels(&help), vec!["tag: string?".to_string()]);
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn imported_std_operation_uses_contextual_callable_signature() {
    let help = help_in_single_file_at(
        "module app\n\
        use std::text\n\
        fn run(): bool\n    \
        return text::contains(\"abc\", |\n",
    )
    .expect("signature help");

    assert_eq!(
        signature_label(&help),
        "std::text::contains(string, string): bool"
    );
    assert_eq!(
        parameter_labels(&help),
        vec!["string".to_string(), "string".to_string()]
    );
    assert_eq!(help.active_parameter, Some(1));
}

#[test]
fn imported_std_operation_fails_closed_for_source_alias_collision() {
    let help = help_in_single_file_at(
        "module app\n\
        use std::text\n\
        resource Book\n    \
        title: string\n\
        store ^books(id: int): Book\n\
        surface text from ^books\n    \
        fields title\n\
        fn run(): bool\n    \
        return text::contains(\"abc\", |\n",
    );

    assert!(help.is_none());
}

#[test]
fn scalar_conversion_uses_canonical_conversion_signature() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(value: string): int\n    return int(|\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "int(value): int");
    assert_eq!(parameter_labels(&help), vec!["value".to_string()]);
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn identity_constructor_uses_canonical_callable_signature() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): Id(^books)\n    return Id(|\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "Id(^root, key...): Id");
    assert_eq!(
        parameter_labels(&help),
        vec!["^root".to_string(), "key...".to_string()]
    );
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn identity_constructor_repeated_key_parameter_stays_active() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): Id(^books)\n    return Id(^books, |\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "Id(^root, key...): Id");
    assert_eq!(help.active_parameter, Some(1));
}

#[test]
fn error_constructor_uses_named_canonical_callable_signature() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): Error\n    return Error(message: |\n",
    )
    .expect("signature help");

    assert_eq!(
        signature_label(&help),
        "Error(code: ErrorCode, message: string, help: string, data: unknown): Error"
    );
    assert_eq!(
        parameter_labels(&help),
        vec![
            "code: ErrorCode".to_string(),
            "message: string".to_string(),
            "help: string".to_string(),
            "data: unknown".to_string(),
        ]
    );
    assert_eq!(help.active_parameter, Some(1));
}

#[test]
fn collection_builtins_use_canonical_collection_shape() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(items: unknown): unknown\n    return reversed(|\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "reversed(collection): sequence");
    assert_eq!(parameter_labels(&help), vec!["collection".to_string()]);
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn non_callable_checker_operations_return_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): absent\n    return write(|\n",
    );

    assert!(help.is_none());
}

#[test]
fn unrelated_file_returns_no_signature_help_for_builtin_call() {
    let (program, file) = project();
    let unrelated = file.with_file_name("scratch.mw");
    let help = help_at(
        &program,
        &unrelated,
        "module scratch\n\npub fn run(): int\n    return int(|\n",
    );

    assert!(help.is_none());
}

#[test]
fn scalar_conversion_in_named_argument_value_keeps_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): Book\n    return Book(title: string(|\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "string(value): string");
    assert_eq!(parameter_labels(&help), vec!["value".to_string()]);
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn builtin_call_in_return_context_keeps_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): int\n    return count(|\n",
    )
    .expect("signature help");

    assert_eq!(signature_label(&help), "count(collection): int");
    assert_eq!(help.active_parameter, Some(0));
}

#[test]
fn builtin_call_in_var_keyed_header_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): int\n    var count(|\n",
    );

    assert!(help.is_none());
}

#[test]
fn scalar_call_in_var_keyed_header_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): int\n    var int(|\n",
    );

    assert!(help.is_none());
}

#[test]
fn declaration_heads_with_builtin_shaped_names_return_no_signature_help() {
    let (program, file) = project();
    let cases = [
        ("const declaration", "module shelf::app\n\nconst count(|\n"),
        (
            "resource declaration",
            "module shelf::app\n\nresource count(|\n",
        ),
        ("enum declaration", "module shelf::app\n\nenum count(|\n"),
        ("module declaration", "module count(|\n"),
        ("use declaration", "module shelf::app\n\nuse count(|\n"),
    ];
    let false_positives = cases
        .iter()
        .filter_map(|(label, source)| {
            help_at(&program, &file, source)
                .map(|help| format!("{label}: {}", signature_label(&help)))
        })
        .collect::<Vec<_>>();

    assert!(
        false_positives.is_empty(),
        "expected no signature help in declaration heads, got {false_positives:?}"
    );
}

#[test]
fn builtin_call_inside_keyed_local_declaration_parens_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): int\n    var seen(count(|\n",
    );

    assert!(help.is_none());
}

#[test]
fn resource_member_key_list_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\nresource Counter\n    count(|): string\n\nstore ^counters: Counter\n",
    );

    assert!(help.is_none());
}

#[test]
fn resource_group_key_list_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\nresource Counter\n    count(|)\n\nstore ^counters: Counter\n",
    );

    assert!(help.is_none());
}

#[test]
fn required_resource_member_key_list_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\nresource Counter\n    required count(|): string\n\nstore ^counters: Counter\n",
    );

    assert!(help.is_none());
}

#[test]
fn resource_index_argument_list_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\nresource Counter\n    index count(|)\n\nstore ^counters: Counter\n",
    );

    assert!(help.is_none());
}

#[test]
fn outside_a_call_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(): int\n    return |\n",
    );

    assert!(help.is_none());
}

#[test]
fn stable_id_metadata_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\nresource Counter\n    @id(|\n    amount: int\n\nstore ^counters: Counter\n",
    );

    assert!(help.is_none());
}

#[test]
fn builtin_call_inside_stable_id_metadata_parens_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\nresource Counter\n    @id(count(|\n    amount: int\n\nstore ^counters: Counter\n",
    );

    assert!(help.is_none());
}

#[test]
fn type_annotation_contexts_return_no_signature_help() {
    let (program, file) = project();
    let cases = [
        (
            "function parameter scalar type",
            "module shelf::app\n\nfn typed(value: int(|\n",
        ),
        (
            "function parameter qualified resource type",
            "module shelf::app\n\nfn typed(value: books::Book(|\n",
        ),
        (
            "function parameter identity type",
            "module shelf::app\n\nfn typed(value: Id(^books)(|\n",
        ),
        (
            "saved root key scalar type",
            "module shelf::app\n\nresource Counter\n\nstore ^counters(id: int(|): Counter\n",
        ),
        (
            "required resource member scalar type",
            "module shelf::app\n\nresource Counter\n    required amount: int(|\n\nstore ^counters: Counter\n",
        ),
        (
            "local variable scalar type",
            "module shelf::app\n\npub fn run(): int\n    var local: int(|\n",
        ),
    ];
    let false_positives = cases
        .iter()
        .filter_map(|(label, source)| {
            help_at(&program, &file, source)
                .map(|help| format!("{label}: {}", signature_label(&help)))
        })
        .collect::<Vec<_>>();

    assert!(
        false_positives.is_empty(),
        "expected no signature help in type annotations, got {false_positives:?}"
    );
}

#[test]
fn function_declaration_parameter_list_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(&program, &file, "module shelf::app\n\nfn add(|\n");

    assert!(help.is_none());
}

#[test]
fn saved_root_declaration_key_list_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\nresource Book\n\nstore ^books(|): Book\n",
    );

    assert!(help.is_none());
}

#[test]
fn field_access_call_returns_no_signature_help() {
    let (program, file) = project();
    let help = help_at(
        &program,
        &file,
        "module shelf::app\n\npub fn run(book: Book): string\n    return book.title(|\n",
    );

    assert!(help.is_none());
}

#[test]
fn keyless_store_does_not_suppress_module_resource_constructor() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let app = src.join("app.mw");
    std::fs::write(
        &app,
        "\
module app

resource Settings
    enabled: bool

store ^settings: Settings
",
    )
    .unwrap();
    std::fs::write(
        src.join("Settings.mw"),
        "\
module Settings

resource Badge
    required value: int
",
    )
    .unwrap();
    let config =
        parse_config(r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#).unwrap();
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None, None).unwrap();

    let help = help_at(
        &snapshot.program,
        &app,
        "module app\n\npub fn run(): Settings::Badge\n    return Settings::Badge(|\n",
    )
    .expect("module resource constructor signature help");

    assert_eq!(signature_label(&help), "Badge(value: int): Badge");
    assert_eq!(parameter_labels(&help), vec!["value: int".to_string()]);
    assert_eq!(help.active_parameter, Some(0));
}
