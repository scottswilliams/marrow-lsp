use super::*;
use lsp_types::HoverContents;
use marrow_check::{ProjectSources, analyze_project};
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
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None).unwrap();
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
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None).unwrap();
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

fn assert_status_enum_hover(value: &str) {
    assert!(
        value.starts_with("```marrow\nenum Status\n```"),
        "enum annotation hover should lead with the enum signature: {value}"
    );
    assert!(
        value.contains("Lifecycle state."),
        "enum annotation hover should include docs: {value}"
    );
    assert!(
        value.contains("open"),
        "enum annotation hover should include member summary: {value}"
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
    let offset = offset_of(source, "book.title") + "book.".len();

    let hover = hover_value_at(&snapshot, &index_for(&snapshot), &file, offset);
    if let Some(value) = hover {
        assert_ne!(value, "```marrow\ntitle: string\n```");
        assert!(
            !value.contains("The parameter, not the field."),
            "field hover should not inherit parameter docs: {value}"
        );
    }
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
    let offset = offset_of(source, "Book(title: title)") + "Book(".len();

    let hover = hover_value_at(&snapshot, &index_for(&snapshot), &file, offset);
    if let Some(value) = hover {
        assert_ne!(value, "```marrow\ntitle: string\n```");
        assert!(
            !value.contains("The parameter, not the named argument label."),
            "named argument label should not inherit parameter docs: {value}"
        );
    }
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
    let (snapshot, file) = analyze(source);
    let offset = offset_of(source, "return n") + "return ".len();

    let hover = hover(&snapshot, &file, offset).expect("a hover at the use of `n`");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup contents");
    };
    assert!(
        markup.value.starts_with("```marrow\nn: string\n```"),
        "duplicate parameter hover should match the binding in scope: {}",
        markup.value
    );
    assert!(
        markup.value.contains("Second parameter docs."),
        "duplicate parameter hover should use the later parameter docs: {}",
        markup.value
    );
    assert!(
        !markup.value.contains("First parameter docs."),
        "duplicate parameter hover should not use the shadowed parameter docs: {}",
        markup.value
    );
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
    let (snapshot, file) = analyze(source);
    let offset = offset_of(source, "return n") + "return ".len();

    let hover = hover(&snapshot, &file, offset).expect("a hover at the use of `n`");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup contents");
    };
    assert!(
        markup.value.starts_with("```marrow\nn: int\n```"),
        "parameter signature should lead the hover: {}",
        markup.value
    );
    assert!(
        markup.value.contains("**Parameter**"),
        "parameter docs should be in a short section: {}",
        markup.value
    );
    assert!(
        markup.value.contains("The number to return."),
        "parameter docs should be shown: {}",
        markup.value
    );
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
    assert_eq!(markup.value, "```marrow\nint\n```");
    assert!(
        !markup.value.contains("The outer parameter."),
        "shadowing local should not inherit parameter docs: {}",
        markup.value
    );
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
    let (snapshot, file) = analyze(source);
    let offset = source.rfind("add(1, 2)").unwrap();

    let hover = hover(&snapshot, &file, offset).expect("a hover at the function call");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup contents");
    };
    assert!(
        markup
            .value
            .starts_with("```marrow\nfn add(x: int, y: int): int\n```"),
        "function signature should lead the hover: {}",
        markup.value
    );
    assert!(
        markup.value.contains("Adds two numbers."),
        "function docs should follow the signature: {}",
        markup.value
    );
    assert!(
        !markup.value.contains("Parameters"),
        "parameter docs section should be omitted when no parameter has docs: {}",
        markup.value
    );
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
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let value = hover_value(&snapshot, &index, &file, source, "touch(1)");

    assert!(
        value.starts_with("```marrow\nfn touch(id: int): string\n```"),
        "function signature should lead the hover: {value}"
    );
    assert!(
        value.contains("**Direct effects**"),
        "function hover should include a direct-effects section: {value}"
    );
    assert!(
        value.contains("- saved reads: Book.title, Book.visits"),
        "direct saved reads should use canonical resource/member facts: {value}"
    );
    assert!(
        value.contains("- saved writes: Book.visits"),
        "direct saved writes should use canonical resource/member facts: {value}"
    );
    assert!(
        value.contains("- transaction"),
        "direct transaction effect should be shown: {value}"
    );
    assert!(
        value.contains("- host: output"),
        "direct host output effect should be shown: {value}"
    );
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
    write(\"a\")
    print(\"b\")
    return count(^books)
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let value = hover_value(&snapshot, &index, &file, source, "exists(^books");

    assert!(
        value.starts_with("```marrow\nexists(path): bool\n```"),
        "builtin signature should lead the hover: {value}"
    );
    assert!(
        value.contains("default library"),
        "builtin hover should identify default-library context: {value}"
    );
    assert!(
        value.contains("builtin"),
        "builtin hover should identify the source kind: {value}"
    );
    assert!(
        value.contains("Returns true when the saved path exists."),
        "builtin hover should include exists docs: {value}"
    );

    let next_id_value = hover_value(&snapshot, &index, &file, source, "nextId(^books");
    assert!(
        next_id_value.starts_with("```marrow\nnextId(^root): Id\n```"),
        "builtin signature should lead the hover: {next_id_value}"
    );
    assert!(
        next_id_value.contains("Returns the next id for a saved root."),
        "builtin hover should include nextId docs: {next_id_value}"
    );
    assert!(
        !next_id_value.contains("Returns true when the saved path exists."),
        "builtin docs should be specific to the hovered builtin: {next_id_value}"
    );

    let write_value = hover_value(&snapshot, &index, &file, source, "write(\"a");
    assert!(
        write_value.contains("Writes rendered text to output without a newline."),
        "write hover should describe output behavior without implying saved-state mutation: {write_value}"
    );
    let print_value = hover_value(&snapshot, &index, &file, source, "print(\"b");
    assert!(
        print_value.contains("Writes rendered text to output with a newline."),
        "print hover should describe output behavior: {print_value}"
    );
    let count_value = hover_value(&snapshot, &index, &file, source, "count(^books");
    assert!(
        count_value
            .contains("Returns child count for a saved path, 1 for a scalar, or 0 when absent."),
        "count hover should describe path/scalar/absent behavior: {count_value}"
    );
}

#[test]
fn hover_over_std_call_leaf_shows_signature_and_capability() {
    let source = "\
module a

pub fn f(): int
    return std::text::length(\"abc\")
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let value = hover_value(&snapshot, &index, &file, source, "length(\"abc");

    assert!(
        value.starts_with("```marrow\nstd::text::length(string): int\n```"),
        "std signature should lead the hover: {value}"
    );
    assert!(
        value.contains("Capability: pure"),
        "std hover should include capability context: {value}"
    );
    assert!(
        value.contains("default library"),
        "std hover should identify default-library context: {value}"
    );
}

#[test]
fn hover_over_scalar_conversion_call_shows_default_library_signature() {
    let source = "\
module a

pub fn f(): int
    return int(\"1\")
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let value = hover_value(&snapshot, &index, &file, source, "int(\"1");

    assert!(
        value.starts_with("```marrow\nint(value): int\n```"),
        "scalar conversion signature should lead the hover: {value}"
    );
    assert!(
        value.contains("conversion"),
        "scalar hover should identify conversion context: {value}"
    );
    assert!(
        value.contains("default library"),
        "scalar hover should identify default-library context: {value}"
    );
}

#[test]
fn hover_over_error_code_conversion_shows_default_library_signature() {
    let source = "\
module a

pub fn f(): ErrorCode
    return ErrorCode(\"not_found\")
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let value = hover_value(&snapshot, &index, &file, source, "ErrorCode(\"not");

    assert!(
        value.starts_with("```marrow\nErrorCode(value): ErrorCode\n```"),
        "ErrorCode conversion signature should use the language type: {value}"
    );
    assert!(
        value.contains("conversion"),
        "ErrorCode hover should identify conversion context: {value}"
    );
    assert!(
        value.contains("default library"),
        "ErrorCode hover should identify default-library context: {value}"
    );
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

    let std_value = hover_value(&snapshot, &index, &file, source, "std::text");
    assert!(
        std_value.starts_with("```marrow\nstd\n```"),
        "std namespace hover should lead with the namespace: {std_value}"
    );
    assert!(
        std_value.contains("default library namespace"),
        "std namespace hover should identify default-library context: {std_value}"
    );

    let text_offset = offset_of(source, "std::text") + "std::".len();
    let text_value = hover_value_at(&snapshot, &index, &file, text_offset)
        .expect("a hover over the std::text module segment");
    assert!(
        text_value.starts_with("```marrow\nstd::text\n```"),
        "std::text default-library docs should lead with the module path: {text_value}"
    );
    assert!(
        text_value.contains("default library module"),
        "std::text hover should identify default-library context: {text_value}"
    );
    assert!(
        text_value.contains("length"),
        "std::text hover should summarize available operations: {text_value}"
    );

    let length_value = hover_value(&snapshot, &index, &file, source, "length(\"abc");
    assert!(
        length_value.starts_with("```marrow\nstd::text::length(string): int\n```"),
        "std operation leaf hover should remain the operation signature: {length_value}"
    );
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
        let std_value = hover_value_at(&snapshot, &index, &file, offset_of(app_source, needle));
        assert!(
            !std_value
                .as_deref()
                .is_some_and(|value| value.contains("default library")),
            "project-owned {needle} std segment should not get default-library docs: {std_value:?}"
        );

        let module_offset = offset_of(app_source, needle) + "std::".len();
        let module_value = hover_value_at(&snapshot, &index, &file, module_offset);
        assert!(
            !module_value
                .as_deref()
                .is_some_and(|value| value.contains("default library")),
            "project-owned {needle} module segment should not get default-library docs: {module_value:?}"
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

    let std_value = hover_value_at(&snapshot, &index, &file, offset_of(app_source, "std::text"));
    assert!(
        std_value
            .as_deref()
            .is_some_and(|value| value.contains("default library namespace")),
        "std segment should keep default-library docs when a builtin std op wins dispatch: {std_value:?}"
    );

    let text_offset = offset_of(app_source, "std::text") + "std::".len();
    let text_value = hover_value_at(&snapshot, &index, &file, text_offset);
    assert!(
        text_value
            .as_deref()
            .is_some_and(|value| value.contains("default library module")),
        "std::text segment should keep default-library docs when a builtin std op wins dispatch: {text_value:?}"
    );

    let length_value = hover_value(&snapshot, &index, &file, app_source, "length(\"abc");
    assert!(
        length_value.starts_with("```marrow\nstd::text::length(string): int\n```"),
        "std::text::length leaf hover should keep builtin precedence over a project function: {length_value}"
    );
    assert!(
        length_value.contains("default library"),
        "std::text::length leaf hover should identify the builtin dispatch: {length_value}"
    );
}

#[test]
fn hover_over_project_module_leaf_in_use_is_blocked_without_canonical_fact() {
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
    assert!(
        hover_value_at(&snapshot, &index, &file, leaf + 1).is_none(),
        "module/use path hover stays unavailable until Marrow exposes canonical module-path facts"
    );
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
    assert!(
        hover_value_at(&snapshot, &index, &file, call + 1).is_none(),
        "module-prefix hover stays blocked until Marrow exposes canonical prefix facts"
    );

    let leaf = call + "books::".len();
    let function =
        hover_value_at(&snapshot, &index, &file, leaf + 1).expect("a hover over the function leaf");
    assert!(
        function.starts_with("```marrow\nfn titleOf(): string\n```"),
        "function leaf should keep the rich function hover: {function}"
    );
    assert!(
        function.contains("Formats a book title."),
        "function leaf should keep function docs: {function}"
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
    assert!(
        hover_value_at(&snapshot, &index, &file, books + 1).is_none(),
        "fully qualified module-prefix hover stays blocked until canonical prefix facts exist"
    );

    let leaf = call + "shelf::books::".len();
    let function =
        hover_value_at(&snapshot, &index, &file, leaf + 1).expect("a hover over the function leaf");
    assert!(
        function.starts_with("```marrow\nfn titleOf(): string\n```"),
        "fully qualified function leaf should keep the function hover: {function}"
    );
    assert!(
        function.contains("Formats a book title."),
        "fully qualified function leaf should keep function docs: {function}"
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
    assert!(
        hover_value_at(&snapshot, &index, &file, qualifier + 1).is_none(),
        "qualified enum module-prefix hover stays blocked until canonical prefix facts exist"
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
    assert!(
        value.contains("Local status enum."),
        "enum path head should keep enum docs: {value}"
    );
    assert!(
        !value.starts_with("```marrow\nmodule status\n```"),
        "enum path head should not be stolen by the same-named module: {value}"
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
    if let Some(value) = hover_value_at(&snapshot, &index, &file, head + 1) {
        assert!(
            !value.starts_with("```marrow\nbook::Id\n```"),
            "removed identity path must not get a generated identity hover: {value}"
        );
        assert!(
            !value.contains("Saved books."),
            "removed identity path must not inherit resource docs: {value}"
        );
    }
}

#[test]
fn hover_over_project_std_text_module_without_builtin_dispatch_is_blocked() {
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
    assert!(
        hover_value_at(&snapshot, &index, &file, text + 1).is_none(),
        "project std::text module-prefix hover stays blocked until canonical prefix facts exist"
    );

    let custom = hover_value_at(
        &snapshot,
        &index,
        &file,
        offset_of(app_source, "custom()") + 1,
    )
    .expect("a hover over the project function leaf");
    assert!(
        custom.starts_with("```marrow\nfn custom(): int\n```"),
        "project function leaf should keep its function hover: {custom}"
    );
    assert!(
        custom.contains("Project-only text helper."),
        "project function leaf should keep function docs: {custom}"
    );
    assert!(
        !custom.contains("default library"),
        "project function leaf should not get default-library docs: {custom}"
    );
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

    let plus = hover_value(&snapshot, &index, &file, source, "+");
    assert!(
        plus.starts_with("```marrow\noperator +\n```"),
        "symbolic operator hover should lead with the operator: {plus}"
    );
    assert!(
        plus.contains("language operator"),
        "symbolic operator hover should identify language-operator context: {plus}"
    );
    assert!(
        plus.contains("addition"),
        "plus hover should describe addition behavior: {plus}"
    );

    let and_value = hover_value(&snapshot, &index, &file, source, "and");
    assert!(
        and_value.starts_with("```marrow\noperator and\n```"),
        "keyword operator hover should lead with the operator: {and_value}"
    );
    assert!(
        and_value.contains("language operator"),
        "keyword operator hover should identify language-operator context: {and_value}"
    );
    assert!(
        and_value.contains("logical"),
        "and hover should describe logical behavior: {and_value}"
    );
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
    let value = hover_value_at(&snapshot, &index, &file, module_offset);

    assert!(
        !value
            .as_deref()
            .is_some_and(|value| value.contains("language operator")),
        "module path segment named like an operator should not get operator docs: {value:?}"
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
    let module_value = hover_value_at(
        &module_snapshot,
        &module_index,
        &module_file,
        offset_of(and_source, "module and") + "module ".len(),
    );
    assert!(
        !module_value
            .as_deref()
            .is_some_and(|value| value.contains("language operator")),
        "module declaration segment named like an operator should not get operator docs: {module_value:?}"
    );

    let (app_snapshot, app_file) =
        analyze_files(&[("and.mw", and_source), ("app.mw", app_source)], "app.mw");
    let app_index = index_for(&app_snapshot);
    let use_value = hover_value_at(
        &app_snapshot,
        &app_index,
        &app_file,
        offset_of(app_source, "use and") + "use ".len(),
    );
    assert!(
        !use_value
            .as_deref()
            .is_some_and(|value| value.contains("language operator")),
        "use segment named like an operator should not get operator docs: {use_value:?}"
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
        let value = hover_value_at(&snapshot, &index, &file, offset);
        assert!(
            !value
                .as_deref()
                .is_some_and(|value| value.contains("language operator")),
            "resource field named `{name}` should not get operator docs: {value:?}"
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
    assert_eq!(
        declaration, "```marrow\nfn exists(value: unknown): bool\n```",
        "declaration hover should remain the user function"
    );
    assert!(
        !declaration.contains("default library"),
        "declaration hover should not include builtin docs: {declaration}"
    );

    let call = hover_value(&snapshot, &index, &file, source, "exists(^books");
    assert!(
        call.starts_with("```marrow\nexists(path): bool\n```"),
        "call hover should use the builtin signature: {call}"
    );
    assert!(
        call.contains("default library"),
        "call hover should identify default-library context: {call}"
    );
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

    let exists_value = hover_value(&snapshot, &index, &file, app_source, "exists()");
    assert!(
        !exists_value.contains("exists(path): bool"),
        "qualified foo::exists call should not get builtin hover: {exists_value}"
    );
    assert!(
        !exists_value.contains("default library"),
        "qualified foo::exists call should not get default-library docs: {exists_value}"
    );

    let length_value = hover_value(&snapshot, &index, &file, app_source, "length(\"abc");
    assert!(
        !length_value.contains("std::text::length(string): int"),
        "qualified foo::std::text::length call should not get std hover: {length_value}"
    );
    assert!(
        !length_value.contains("default library"),
        "qualified foo::std::text::length call should not get default-library docs: {length_value}"
    );

    let std_offset = offset_of(app_source, "std::text::length");
    let std_value = hover_value_at(&snapshot, &index, &file, std_offset);
    assert!(
        !std_value
            .as_deref()
            .is_some_and(|value| value.contains("default library")),
        "qualified foo::std::text std segment should not get default-library docs: {std_value:?}"
    );

    let text_offset = offset_of(app_source, "std::text::length") + "std::".len();
    let text_value = hover_value_at(&snapshot, &index, &file, text_offset);
    assert!(
        !text_value
            .as_deref()
            .is_some_and(|value| value.contains("default library")),
        "qualified foo::std::text text segment should not get default-library docs: {text_value:?}"
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
    let offset = offset_of(source, "n():") + 1;

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
    let (snapshot, file) = analyze(source);
    let offset = source.rfind("add(1, 2)").unwrap();

    let hover = hover(&snapshot, &file, offset).expect("a hover at the function call");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup contents");
    };
    assert!(
        markup
            .value
            .starts_with("```marrow\nfn add(x: int, y: int): int\n```"),
        "function signature should lead the hover: {}",
        markup.value
    );
    assert!(
        markup.value.contains("**Parameters**"),
        "parameter docs section should be present: {}",
        markup.value
    );
    assert!(
        markup.value.contains("- `x`: The left value."),
        "first parameter docs should be shown: {}",
        markup.value
    );
    assert!(
        markup.value.contains("- `y`: The right value."),
        "second parameter docs should be shown: {}",
        markup.value
    );
}

#[test]
fn hover_over_a_resource_declaration_name_shows_docs_and_members_without_store_identity() {
    let source = "\
module a

;; Book records.
resource Book
    ;; Display title.
    required title: string
    pages: int
    tags(pos: int): string

;; Books saved by id.
store ^books(id: int): Book
    index byTitle(title) unique
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);
    let value = hover_value(&snapshot, &index, &file, source, "Book\n    ;; Display");

    assert!(
        value.starts_with("```marrow\nresource Book\n```"),
        "resource signature should lead the hover: {value}"
    );
    assert!(
        value.contains("Book records."),
        "resource docs should be shown: {value}"
    );
    assert!(
        !value.contains("Books saved by id."),
        "resource hover should not inherit store docs: {value}"
    );
    assert!(
        !value.contains("Id(^books)"),
        "resource hover should not name the store-owned identity type: {value}"
    );
    assert!(
        value.contains("required title: string"),
        "field summary should include member type: {value}"
    );
    assert!(
        !value.contains("title: string required"),
        "field summary should preserve resource syntax order: {value}"
    );
    assert!(
        value.contains("tags(pos: int): string"),
        "keyed leaf summary should include key and value type: {value}"
    );
    assert!(
        !value.contains("index byTitle(title) unique"),
        "resource hover should not include store-owned indexes: {value}"
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
    let value = hover_value(&snapshot, &index, &file, source, "Book(title");

    assert!(
        value.starts_with("```marrow\nresource Book\n```"),
        "constructor should show the resource declaration, not only the type: {value}"
    );
    assert!(
        value.contains("Books saved by id."),
        "constructor hover should include resource docs: {value}"
    );
    assert!(
        value.contains("title: string"),
        "constructor hover should include member summary: {value}"
    );
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
    assert!(
        value.contains("Books from state."),
        "qualified resource constructor should include docs: {value}"
    );
    assert!(
        value.contains("required title: string"),
        "qualified resource constructor should include members: {value}"
    );
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
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nresource Book\n```"),
            "resource annotation hover should lead with resource signature: {value}"
        );
        assert!(
            value.contains("Books saved by id."),
            "resource annotation hover should include resource docs: {value}"
        );
        assert!(
            !value.contains("Id(^books)"),
            "resource annotation hover should not name the store-owned identity type: {value}"
        );
        assert!(
            value.contains("required title: string"),
            "resource annotation hover should include member summary: {value}"
        );
    }
}

#[test]
fn hover_over_evolve_transform_type_annotation_shows_resource_hover() {
    let source = "\
module a

;; Books saved by id.
resource Book
    required title: string

store ^books(id: int): Book

evolve
    transform ^books
        const draft: Book = Book(title: \"x\")
";
    let (snapshot, file) = analyze(source);
    let index = index_for(&snapshot);

    let offset = offset_of(source, "draft: Book") + "draft: ".len();
    let hover = hover_with_index(&snapshot, &index, &file, offset).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    let value = markup.value;

    assert!(
        value.starts_with("```marrow\nresource Book\n```"),
        "evolve transform type annotation should show resource hover: {value}"
    );
    assert!(
        value.contains("Books saved by id."),
        "resource docs should be included: {value}"
    );
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
    assert!(
        value.contains("Second module books."),
        "resource annotation hover should include the current module docs: {value}"
    );
    assert!(
        value.contains("subtitle: string"),
        "resource annotation hover should include the current module members: {value}"
    );
    assert!(
        !value.contains("First module books."),
        "resource annotation hover should not use the other module docs: {value}"
    );
    assert!(
        !value.contains("id: int"),
        "resource annotation hover should not use the other module store keys: {value}"
    );
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
    assert!(
        value.contains("Books from state."),
        "wrapped resource annotation hover should include foreign docs: {value}"
    );
    assert!(
        value.contains("required title: string"),
        "wrapped resource annotation hover should include foreign members: {value}"
    );
    assert!(
        !value.contains("code: string"),
        "wrapped resource annotation hover should not use app-local same-named resource: {value}"
    );
    assert!(
        !value.contains("App-local books."),
        "wrapped resource annotation hover should not include app-local docs: {value}"
    );
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
    let value = hover_value(&snapshot, &index, &file, second_source, "Book");

    assert!(
        value.starts_with("```marrow\nresource Book\n```"),
        "resource hover should use the declaring module schema: {value}"
    );
    assert!(
        value.contains("Second module books."),
        "resource hover should include the second module docs: {value}"
    );
    assert!(
        value.contains("subtitle: string"),
        "resource hover should include the second module members: {value}"
    );
    assert!(
        !value.contains("First module books."),
        "resource hover should not use the first module docs: {value}"
    );
    assert!(
        !value.contains("id: int"),
        "resource hover should not use the first module store keys: {value}"
    );
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
        let value = markup.value;

        assert!(
            value.starts_with("```marrow\nstore ^books(id: int): Book\n```"),
            "saved root hover should show the store signature: {value}"
        );
        assert!(
            value.contains("Books saved by id."),
            "saved root hover should include store docs: {value}"
        );
        assert!(
            value.contains("Id(^books)"),
            "saved root hover should include the store-owned identity type: {value}"
        );
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
        value.contains("Display title."),
        "saved field hover should keep member docs: {value}"
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
        value.contains("Books saved by id."),
        "saved root hover should include store docs: {value}"
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
    let value = hover_value(&snapshot, &index, &file, source, "Book");

    assert!(
        value.contains("- `five: string`"),
        "fifth member should be shown: {value}"
    );
    assert!(
        !value.contains("- `six: string`"),
        "sixth member should be hidden behind the bound: {value}"
    );
    assert!(
        value.contains("- ... 1 more"),
        "bounded summary should report the hidden member count: {value}"
    );
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
    let value = hover_value(&snapshot, &index, &file, source, "Book");

    assert!(
        value.contains("- `details`"),
        "group itself should be shown: {value}"
    );
    assert!(
        value.contains("- `required details.subtitle: string`"),
        "nested required field should be summarized with its path: {value}"
    );
    assert!(
        value.contains("- `details.pages: int`"),
        "nested plain field should be summarized with its path: {value}"
    );
    assert!(
        value.contains("- `editions(number: int)`"),
        "keyed layer itself should be shown: {value}"
    );
    assert!(
        value.contains("- `editions(number: int).isbn: string`"),
        "nested keyed-layer member should be shown before the bound: {value}"
    );
    assert!(
        !value.contains("- `editions(number: int).format: string`"),
        "sixth flattened member should be hidden behind the bound: {value}"
    );
    assert!(
        value.contains("- ... 1 more"),
        "bounded nested summary should report the hidden member count: {value}"
    );
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
        value.contains("The displayed title."),
        "field docs should follow the member signature: {value}"
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
        value.contains("The displayed title."),
        "field docs should follow the member signature: {value}"
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
        value.contains("Books saved by id."),
        "declared saved root should include store docs: {value}"
    );
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
    let value = hover_value(&snapshot, &index, &file, source, "^settings");
    assert!(
        value.starts_with("```marrow\nstore ^settings: Settings\n```"),
        "keyless store hover should lead with the store signature: {value}"
    );
    assert!(
        value.contains("no generated identity type"),
        "keyless store hover should say it has no generated identity type: {value}"
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
    let value = hover_value(&snapshot, &index, &file, source, "Status");

    assert!(
        value.starts_with("```marrow\nenum Status\n```"),
        "enum signature should lead the hover: {value}"
    );
    assert!(
        value.contains("Lifecycle state."),
        "enum docs should be shown: {value}"
    );
    assert!(
        value.contains("open"),
        "enum member summary should include open: {value}"
    );
    assert!(
        value.contains("closed"),
        "enum member summary should include closed: {value}"
    );
    assert!(
        !value.contains("ordinal"),
        "enum hover should not include source-order ordinals: {value}"
    );
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
    let hover = hover_with_index(&snapshot, &index, &file, offset).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    assert_status_enum_hover(&markup.value);
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
    let value = hover_value_at(&snapshot, &index, &file, offset).expect("a hover");
    assert_status_enum_hover(&value);
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
    let value = hover_value_at(&snapshot, &index, &file, offset).expect("a hover");
    assert_status_enum_hover(&value);
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
    let value = hover_value_at(&snapshot, &index, &file, offset);

    assert!(
        !value.as_deref().is_some_and(|value| {
            value.contains("First lifecycle state.") || value.contains("Second lifecycle state.")
        }),
        "ambiguous bare enum annotation should not pick a foreign enum: {value:?}"
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
    let value = hover_value_at(&snapshot, &index, &file, offset);

    assert!(
        !value
            .as_deref()
            .is_some_and(|value| value.contains("Private lifecycle state.")),
        "private foreign enum annotation should not show enum docs: {value:?}"
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
    assert!(
        value.contains("enum Status"),
        "enum member hover should name the enum: {value}"
    );
    assert!(
        !value.contains("ordinal"),
        "enum member hover should not include source-order ordinals: {value}"
    );
    assert!(
        value.contains("selectable"),
        "leaf enum member should be marked selectable: {value}"
    );
    assert!(
        value.contains("Open for edits."),
        "enum member docs should be shown: {value}"
    );
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
    let value = hover_value(&snapshot, &index, &file, source, "add(1, 2)");

    assert!(
        value.starts_with("```marrow\nfn add(x: int, y: int): int\n```"),
        "function hovers should keep leading with the signature: {value}"
    );
    assert!(
        value.contains("Adds two numbers."),
        "function docs should still be present: {value}"
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
    let call = app_source.rfind("math::add(1, 2)").unwrap();
    let qualifier_hover = hover(&snapshot, &file, call + 1);
    if let Some(hover) = qualifier_hover {
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert!(
            !markup.value.contains("fn add("),
            "module qualifier hover should not pretend to be the function: {}",
            markup.value
        );
        assert!(
            !markup.value.contains("Adds two numbers."),
            "module qualifier hover should not inherit function docs: {}",
            markup.value
        );
    }

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
    let source = "\
module a

resource Book
    required title: string

store ^books(id: int): Book

pub fn f(): string
    return ^books(1).title ?? \"\"
";
    let (snapshot, file) = analyze(source);
    let offset = source.rfind(").title").unwrap() + ").".len() + 1;
    let hover = hover_with_docs(&snapshot, &file, offset, None).expect("a hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    assert_eq!(markup.value, "```marrow\nstring\n```");
}

/// Build the binding index for a snapshot so a docs test can resolve a symbol.
fn index_for(snapshot: &AnalysisSnapshot) -> BindingIndex {
    marrow_check::build_binding_index(snapshot)
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
fn hover_over_a_documented_function_shows_type_and_description() {
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
    let offset = source.rfind("add(1, 2)").unwrap();
    let docs = symbol_docs(&snapshot, &index, &file, offset);
    let hover =
        hover_with_docs(&snapshot, &file, offset, docs.as_deref()).expect("a hover at the call");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markup");
    };
    assert!(
        markup.value.starts_with("```marrow\n"),
        "the type still leads the hover: {}",
        markup.value
    );
    assert!(
        markup.value.contains("Adds two numbers."),
        "the description should be shown: {}",
        markup.value
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
        value.contains("Maximum count."),
        "the description should be shown: {value}"
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
