use super::*;
use marrow_check::{CheckedModule, ProjectSources, analyze_project};
use marrow_project::parse_config;
use marrow_syntax::{lex_source, parse_source};

/// A two-file project: `shelf::books` declares the `Book` resource, a store,
/// and a public helper; `shelf::app` uses it. Returns the checked program and
/// the path of the file completion runs in (the `app` file).
fn project() -> (CheckedProgram, std::path::PathBuf) {
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

resource Book
    ;; Display title.
    required title: string
    required shelf: string
    tags(pos: int): string

    notes(noteId: string)
        text: string

;; Books saved by id.
store ^books(id: int): Book
    index byShelf(shelf, id)

resource Pair
    required label: string

store ^pairs(left: int, right: int): Pair

;; Lifecycle state.
pub enum Status
    ;; Ready for use.
    active
    ;; No longer active.
    archived

;; Returns a book title.
pub fn titleOf(id: Id(^books)): string
    return ^books(id).title

const LIMIT: int = 100
",
    )
    .unwrap();
    let app = pkg.join("app.mw");
    std::fs::write(
        &app,
        "\
module shelf::app

use shelf::books

pub fn run(count: int): int
    const total: int = count
    return total
",
    )
    .unwrap();
    let config =
        parse_config(r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#).unwrap();
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None).unwrap();
    std::mem::forget(dir);
    (snapshot.program, app)
}

fn same_named_enum_project() -> (CheckedProgram, std::path::PathBuf) {
    let app = std::path::PathBuf::from("/project/src/shelf/app.mw");
    let library = std::path::PathBuf::from("/project/src/shelf/private.mw");
    let enum_schema = |member: &str| EnumSchema {
        name: "Status".to_string(),
        docs: Vec::new(),
        members: vec![marrow_schema::EnumMemberSchema {
            name: member.to_string(),
            docs: Vec::new(),
            parent: None,
            category: false,
        }],
    };
    let module = |name: &str, source_file: std::path::PathBuf, member: &str, public: bool| {
        let mut enum_public = std::collections::HashMap::new();
        enum_public.insert("Status".to_string(), public);
        CheckedModule {
            name: name.to_string(),
            source_file,
            span: Default::default(),
            imports: Vec::new(),
            constants: Vec::new(),
            functions: Vec::new(),
            resources: Vec::new(),
            stores: Vec::new(),
            enums: vec![enum_schema(member)],
            enum_public,
        }
    };
    (
        CheckedProgram::from_modules(vec![
            module("shelf::private", library, "foreignOnly", false),
            module("shelf::app", app.clone(), "currentOnly", false),
        ]),
        app,
    )
}

fn current_enum_project() -> (CheckedProgram, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let pkg = root.join("src/shelf");
    std::fs::create_dir_all(&pkg).unwrap();
    let app = pkg.join("app.mw");
    std::fs::write(
        &app,
        "\
module shelf::app

;; Lifecycle state.
enum Status
    ;; Ready for use.
    active
    ;; No longer active.
    archived

pub fn run()
    return
",
    )
    .unwrap();
    let config =
        parse_config(r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#).unwrap();
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None).unwrap();
    std::mem::forget(dir);
    (snapshot.program, app)
}

/// Run completion on `source` at the cursor marked `|`, against `program` for
/// the file at `file`. The marker is stripped before lexing.
fn complete_items(
    program: &CheckedProgram,
    file: &std::path::Path,
    source: &str,
) -> Vec<CompletionItem> {
    let offset = source.find('|').expect("a cursor marker `|`");
    let source = source.replacen('|', "", 1);
    let parsed = parse_source(&source);
    let lexed = lex_source(&source);
    completion(program, file, &source, &parsed, &lexed, offset)
}

fn complete(program: &CheckedProgram, file: &std::path::Path, source: &str) -> Vec<String> {
    complete_items(program, file, source)
        .into_iter()
        .map(|item| item.label)
        .collect()
}

fn item_named<'a>(items: &'a [CompletionItem], label: &str) -> &'a CompletionItem {
    items
        .iter()
        .find(|item| item.label == label)
        .unwrap_or_else(|| panic!("expected completion {label:?}, got {items:?}"))
}

fn documentation_value(item: &CompletionItem) -> &str {
    match item.documentation.as_ref().expect("completion docs") {
        Documentation::MarkupContent(markup) => &markup.value,
        Documentation::String(value) => value,
    }
}

#[test]
fn after_caret_lists_durable_roots() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    delete ^|\n",
    );
    let books = item_named(&items, "books");
    assert_eq!(books.kind, Some(CompletionItemKind::STRUCT));
    assert_eq!(books.detail.as_deref(), Some("saved root of Book"));
    assert_eq!(documentation_value(books), "Books saved by id.");
}

#[test]
fn after_keyed_root_dot_lists_declared_saved_path_members() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id).|\n",
    );
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert_eq!(labels, ["title", "shelf", "tags", "notes"]);

    let title = item_named(&items, "title");
    assert_eq!(title.kind, Some(CompletionItemKind::FIELD));
    assert_eq!(title.detail.as_deref(), Some("required field: string"));

    let shelf = item_named(&items, "shelf");
    assert_eq!(shelf.kind, Some(CompletionItemKind::FIELD));
    assert_eq!(shelf.detail.as_deref(), Some("required field: string"));

    let tags = item_named(&items, "tags");
    assert_eq!(
        tags.kind,
        Some(CompletionItemKind::FIELD),
        "keyed scalar fields must stay field completions"
    );
    assert_eq!(tags.detail.as_deref(), Some("field(pos: int): string"));

    let notes = item_named(&items, "notes");
    assert_eq!(notes.kind, Some(CompletionItemKind::STRUCT));
    assert_eq!(notes.detail.as_deref(), Some("layer(noteId: string)"));
}

#[test]
fn after_single_key_root_dot_accepts_trailing_comma() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id,).|\n",
    );
    assert_eq!(labels, ["title", "shelf", "tags", "notes"]);
}

#[test]
fn after_saved_layer_dot_lists_declared_nested_members() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\npub fn f(id: int, n: string)\n    const x = ^books(id).notes(n).|\n",
    );
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert_eq!(labels, ["text"]);

    let text = item_named(&items, "text");
    assert_eq!(text.kind, Some(CompletionItemKind::FIELD));
    assert_eq!(text.detail.as_deref(), Some("field: string"));
}

#[test]
fn after_saved_layer_dot_recovers_parenthesized_receiver_arguments() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f(id: int, n: string)\n    const x = ^books((id)).notes((n)).|\n",
    );
    assert_eq!(labels, ["text"]);
}

#[test]
fn after_unkeyed_root_dot_lists_no_schema_members_before_identity_keys() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x = ^books.|\n",
    );
    assert!(
        labels.is_empty(),
        "root prefix must not offer declared members before identity keys, got {labels:?}"
    );
}

#[test]
fn after_two_key_root_dot_lists_declared_members_when_arguments_are_present() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f(left: int, right: int)\n    const x = ^pairs(left, right).|\n",
    );

    assert_eq!(labels, ["label"]);
}

#[test]
fn after_composite_identity_root_dot_lists_declared_members() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f(pair: Id(^pairs))\n    const x = ^pairs(pair).|\n",
    );

    assert_eq!(labels, ["label"]);
}

#[test]
fn wrong_identity_argument_returns_no_declared_members() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f(pair: Id(^pairs))\n    const x = ^books(pair).|\n",
    );

    assert!(
        labels.is_empty(),
        "wrong identity argument must not produce declared members, got {labels:?}"
    );
}

#[test]
fn after_two_key_root_dot_returns_empty_when_arguments_are_missing_or_malformed() {
    let (program, file) = project();

    for source in [
        "module shelf::app\n\npub fn f(left: int, right: int)\n    const x = ^pairs(left).|\n",
        "module shelf::app\n\npub fn f(left: int, right: int)\n    const x = ^pairs(left,).|\n",
        "module shelf::app\n\npub fn f(left: int, right: int)\n    const x = ^pairs(,right).|\n",
        "module shelf::app\n\npub fn f(left: int, right: int)\n    const x = ^pairs(left,,right).|\n",
    ] {
        let labels = complete(&program, &file, source);
        assert!(
            labels.is_empty(),
            "incomplete or malformed key arguments must fail closed, got {labels:?} for {source:?}"
        );
    }
}

#[test]
fn malformed_saved_path_member_prefixes_return_no_bare_completions() {
    let (program, file) = project();

    for source in [
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id).return|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id).\"title\"|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id).123|\n",
    ] {
        let labels = complete(&program, &file, source);
        assert!(
            labels.is_empty(),
            "malformed saved member prefix must fail closed, got {labels:?} for {source:?}"
        );
    }
}

#[test]
fn malformed_saved_path_looking_dots_return_no_bare_completions() {
    let (program, file) = project();

    for source in [
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id)).|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books((id).|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id)..|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id)...|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id)..=|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id).return.|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id).\"title\".|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id).123.|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id)..foo.|\n",
    ] {
        let labels = complete(&program, &file, source);
        assert!(
            labels.is_empty(),
            "malformed saved path must fail closed, got {labels:?} for {source:?}"
        );
    }
}

#[test]
fn ordinary_non_saved_dot_keeps_bare_completion() {
    let (program, file) = project();

    for source in [
        "module shelf::app\n\npub fn f(foo: int)\n    const x = foo.|\n",
        "module shelf::app\n\npub fn f(foo: int)\n    const x = foo.return|\n",
        "module shelf::app\n\npub fn f(foo: int, id: int)\n    const x = wrap(^books(id)).|\n",
    ] {
        let labels = complete(&program, &file, source);
        assert!(
            labels.contains(&"return".to_string()),
            "ordinary non-saved dot should keep bare keyword completion, got {labels:?} for {source:?}"
        );
        assert!(
            labels.contains(&"foo".to_string()),
            "ordinary non-saved dot should keep local completion, got {labels:?} for {source:?}"
        );
    }
}

#[test]
fn bare_identifier_lists_locals_and_keywords() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f(count: int)\n    const total: int = 1\n    return t|\n",
    );
    assert!(
        labels.contains(&"count".to_string()),
        "a param local, got {labels:?}"
    );
    assert!(
        labels.contains(&"total".to_string()),
        "a const local, got {labels:?}"
    );
    assert!(
        labels.contains(&"return".to_string()),
        "a keyword, got {labels:?}"
    );
    assert!(
        labels.contains(&"match".to_string()),
        "a match keyword, got {labels:?}"
    );
    assert!(
        labels.contains(&"exists".to_string()),
        "a builtin, got {labels:?}"
    );
}

#[test]
fn bare_identifier_lists_statement_keywords_without_reserved_non_statements() {
    let (program, file) = project();
    let labels = complete(&program, &file, "module shelf::app\n\npub fn f()\n    |\n");

    for keyword in ["return", "transaction", "throw", "try"] {
        assert!(
            labels.contains(&keyword.to_string()),
            "a statement keyword {keyword:?}, got {labels:?}"
        );
    }

    let reserved_non_statements: Vec<&str> = ["merge", "lock"]
        .into_iter()
        .filter(|keyword| labels.contains(&keyword.to_string()))
        .collect();
    assert!(
        reserved_non_statements.is_empty(),
        "reserved non-statement words should not be bare completions, got {reserved_non_statements:?} in {labels:?}"
    );
}

#[test]
fn bare_identifier_lists_checker_single_name_builtins() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    return |\n",
    );

    for builtin in ["reversed", "next", "prev", "Error"] {
        assert!(
            labels.contains(&builtin.to_string()),
            "checker builtin {builtin:?} should be offered, got {labels:?}"
        );
    }
}

#[test]
fn resource_namespace_does_not_list_identity() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x: Book::|\n",
    );
    assert!(
        !labels.contains(&"Id".to_string()),
        "`Book::` must not offer stale resource-owned identities, got {labels:?}"
    );
}

#[test]
fn used_module_qualified_resource_namespace_does_not_list_identity() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const x: books::Book::|\n",
    );
    assert!(
        !labels.contains(&"Id".to_string()),
        "`books::Book::` must not offer stale resource-owned identities, got {labels:?}"
    );
}

#[test]
fn fully_qualified_resource_namespace_does_not_list_identity() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x: shelf::books::Book::|\n",
    );
    assert!(
        !labels.contains(&"Id".to_string()),
        "`shelf::books::Book::` must not offer stale resource-owned identities, got {labels:?}"
    );
}

#[test]
fn foreign_bare_enum_namespace_is_blocked_without_canonical_fact() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x = Status::|\n",
    );
    assert!(
        labels.is_empty(),
        "foreign enum namespace completion should stay blocked, got {labels:?}"
    );
}

#[test]
fn same_module_enum_member_completion_keeps_kind_detail_and_docs() {
    let (program, file) = current_enum_project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x = Status::|\n",
    );
    let active = item_named(&items, "active");
    assert_eq!(active.kind, Some(CompletionItemKind::ENUM_MEMBER));
    assert_eq!(active.detail.as_deref(), Some("Status"));
    assert_eq!(documentation_value(active), "Ready for use.");
}

#[test]
fn bare_enum_namespace_prefers_current_module_without_leaking_private_foreign_enum() {
    let (program, file) = same_named_enum_project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x = Status::|\n",
    );

    assert_eq!(labels, ["currentOnly"]);
}

#[test]
fn used_module_qualified_enum_namespace_is_blocked_without_canonical_module_prefix_fact() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const x = books::Status::|\n",
    );
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        labels.is_empty(),
        "module-prefix enum completion should be blocked, got {labels:?}"
    );
}

#[test]
fn fully_qualified_enum_namespace_is_blocked_without_canonical_module_prefix_fact() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x = shelf::books::Status::|\n",
    );
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert!(
        labels.is_empty(),
        "module-prefix enum completion should be blocked, got {labels:?}"
    );
}

#[test]
fn std_module_namespace_lists_ops() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x = std::clock::|\n",
    );
    assert!(
        labels.contains(&"now".to_string()),
        "a clock op, got {labels:?}"
    );
    assert!(
        labels.contains(&"today".to_string()),
        "a clock op, got {labels:?}"
    );
    assert!(
        !labels.contains(&"length".to_string()),
        "ops from other modules must not leak in, got {labels:?}"
    );
}

#[test]
fn std_root_namespace_lists_modules() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x = std::|\n",
    );
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "text", "bytes", "hash", "math", "json", "csv", "id", "random", "context", "audit",
            "error", "matrix", "clock", "env", "io", "assert", "log"
        ]
    );
    for op in ["now", "length"] {
        assert!(
            !labels.contains(&op),
            "std root should not offer op {op:?}, got {labels:?}"
        );
    }

    let clock = item_named(&items, "clock");
    assert_eq!(clock.kind, Some(CompletionItemKind::MODULE));
    assert_eq!(clock.detail.as_deref(), Some("std module"));
}

#[test]
fn used_module_function_completion_is_blocked_without_canonical_module_prefix_fact() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const x = books::|\n",
    );
    assert!(
        items.is_empty(),
        "module-prefix completion should be blocked, got {items:?}"
    );
}

#[test]
fn used_module_namespace_lists_no_members_without_canonical_module_prefix_fact() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const x = books::|\n",
    );
    assert!(
        labels.is_empty(),
        "module-prefix completion should be blocked, got {labels:?}"
    );
}

#[test]
fn type_position_lists_resources_and_builtin_types() {
    let (program, file) = project();
    let labels = complete(&program, &file, "module shelf::app\n\npub fn f(x: |\n");
    assert!(
        labels.contains(&"int".to_string()),
        "a builtin type, got {labels:?}"
    );
    assert!(
        labels.contains(&"Book".to_string()),
        "a resource, got {labels:?}"
    );
    assert!(
        labels.contains(&"Id(^books)".to_string()),
        "a store identity, got {labels:?}"
    );
    assert!(
        labels.contains(&"Status".to_string()),
        "an enum type, got {labels:?}"
    );
}

#[test]
fn completion_works_with_a_parse_error_elsewhere_in_the_file() {
    let (program, file) = project();
    // A broken declaration above the cursor: the file does not parse cleanly,
    // but the lexer recovers and the cached snapshot program still has `Book`.
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn broken(:\n\npub fn ok()\n    delete ^|\n",
    );
    assert!(
        labels.contains(&"books".to_string()),
        "completion must survive a parse error elsewhere, got {labels:?}"
    );
}
