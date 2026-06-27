use super::*;
use marrow_check::{CheckedModule, ProjectSources, analyze_project};
use marrow_project::parse_config;
use marrow_schema::EnumSchema;
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

;; Book resource docs.
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
    category archived
        ;; No longer active.
        retired

enum Secret
    hidden

;; Returns a book title.
pub fn titleOf(id: Id(^books)): string
    return ^books(id).title

;; Keeps a lifecycle state.
pub fn chooseStatus(label: string, state: Status): Status
    return state

fn privateTitle(): string
    return \"hidden\"

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

;; Draft resource docs.
resource Draft
    required title: string
    required state: books::Status

enum LocalSecret
    draft

pub fn run(count: int): int
    const total: int = count
    return total
",
    )
    .unwrap();
    let config =
        parse_config(r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#).unwrap();
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None, None).unwrap();
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
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None, None).unwrap();
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
fn root_completion_has_no_local_saved_root_model() {
    let source = include_str!("../completion.rs");

    assert!(
        !source.contains("saved_root_stores"),
        "root completion must not keep a local saved-root store walker"
    );
    assert!(
        !source.contains("module.stores"),
        "root completion must not model saved-root candidates by walking checked modules"
    );
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
fn after_grouped_saved_receiver_dot_lists_declared_members() {
    let (program, file) = project();

    for source in [
        "module shelf::app\n\npub fn f(id: int)\n    const x = (^books(id)).|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ((^books(id))).|\n",
    ] {
        let labels = complete(&program, &file, source);
        assert_eq!(
            labels,
            ["title", "shelf", "tags", "notes"],
            "grouped saved receiver should recover declared members for {source:?}"
        );
    }
}

#[test]
fn after_grouped_saved_layer_dot_lists_declared_nested_members() {
    let (program, file) = project();

    for source in [
        "module shelf::app\n\npub fn f(id: int, n: string)\n    const x = (^books(id).notes(n)).|\n",
        "module shelf::app\n\npub fn f(id: int, n: string)\n    const x = (^books(id)).notes(n).|\n",
    ] {
        let labels = complete(&program, &file, source);
        assert_eq!(
            labels,
            ["text"],
            "grouped saved layer receiver should recover declared members for {source:?}"
        );
    }
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
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id)..|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id)...|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id)..=|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id). .|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id).?.|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id).return.|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id).\"title\".|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id).123.|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id)..foo.|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id)[0].|\n",
        "module shelf::app\n\npub fn f(id: int)\n    const x = ^books(id)[0.|\n",
        "module shelf::app\n\npub fn f(id: int, foo: int)\n    const x = ^books(id)(foo.|\n",
        "module shelf::app\n\npub fn f(id: int, foo: int)\n    const x = ^books(id).notes(foo)(foo.|\n",
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
        "module shelf::app\n\npub fn f(foo: int, id: int)\n    const x = wrap(^books(id), foo.|\n",
        "module shelf::app\n\npub fn f(foo: int)\n    const x = ^books(foo.|\n",
        "module shelf::app\n\npub fn f(foo: int)\n    const x = ^books((foo).|\n",
        "module shelf::app\n\npub fn f(id: int, foo: string)\n    const x = ^books(id).notes(foo.|\n",
        "module shelf::app\n\npub fn f(id: int, foo: string)\n    const x = ^books(id).notes((foo).|\n",
        "module shelf::app\n\npub fn f(id: int, foo: int)\n    const broken = ^books(id)(\n    const x = foo.|\n",
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
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f(count: int)\n    const total: int = 1\n    return t|\n",
    );
    let labels = items
        .iter()
        .map(|item| item.label.clone())
        .collect::<Vec<_>>();
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
    let books = item_named(&items, "books");
    assert_eq!(books.kind, Some(CompletionItemKind::MODULE));
    assert_eq!(books.detail.as_deref(), Some("module shelf::books"));
    let std = item_named(&items, "std");
    assert_eq!(std.kind, Some(CompletionItemKind::MODULE));
    assert_eq!(std.detail.as_deref(), Some("standard library"));
    let draft = item_named(&items, "Draft");
    assert_eq!(draft.kind, Some(CompletionItemKind::STRUCT));
    assert_eq!(draft.detail.as_deref(), Some("resource"));
    let run = item_named(&items, "run");
    assert_eq!(run.kind, Some(CompletionItemKind::FUNCTION));
    assert_eq!(run.detail.as_deref(), Some("fn run(count: int): int"));
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
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    return |\n",
    );
    let labels = items
        .iter()
        .map(|item| item.label.clone())
        .collect::<Vec<_>>();

    for builtin in ["reversed", "next", "prev", "key", "Error"] {
        assert!(
            labels.contains(&builtin.to_string()),
            "checker builtin {builtin:?} should be offered, got {labels:?}"
        );
    }
    assert!(
        !labels.contains(&"write".to_string()),
        "removed builtins must not be offered, got {labels:?}"
    );

    let key = item_named(&items, "key");
    assert_eq!(key.kind, Some(CompletionItemKind::FUNCTION));
    assert_eq!(key.detail.as_deref(), Some("key(id): value"));
}

#[test]
fn completion_has_no_local_intrinsic_callable_model() {
    let source = include_str!("../completion.rs");
    for forbidden in [
        "bare_function_builtins",
        "scalar_conversion_names",
        "scalar_conversion_detail",
    ] {
        assert!(
            !source.contains(forbidden),
            "completion must not keep a local intrinsic callable model through {forbidden}"
        );
    }
}

#[test]
fn completion_has_no_local_type_completion_candidate_model() {
    let source = include_str!("../completion.rs");
    for forbidden in ["TYPE_NAMES", "fn resources(", "fn stores(", "fn enums("] {
        assert!(
            !source.contains(forbidden),
            "completion must not keep a local type completion model through {forbidden}"
        );
    }
}

#[test]
fn completion_has_no_local_std_namespace_completion_model() {
    let source = include_str!("../completion.rs");
    for forbidden in [
        "marrow_schema::stdlib",
        "std_root_module_completions",
        "std_module_completions",
        "std_module_exists",
        "ParamType",
        "ReturnType",
        "fn std_signature(",
    ] {
        assert!(
            !source.contains(forbidden),
            "completion must not keep a local std namespace completion model through {forbidden}"
        );
    }
}

#[test]
fn completion_has_no_local_cursor_recovery_model() {
    let source = include_str!("../completion.rs");
    for forbidden in [
        "TokenKind",
        "marrow_syntax::Token",
        "lexed.tokens",
        ".tokens.iter()",
        "significant_tokens",
        "matching_open_",
        "saved_receiver",
        "qualifier_before",
        "introduces_type",
    ] {
        assert!(
            !source.contains(forbidden),
            "completion must not own cursor recovery through {forbidden}"
        );
    }
    assert!(
        source.contains("source_completion_fact"),
        "completion must request Marrow's aggregate completion fact"
    );
}

#[test]
fn completion_consumes_marrow_completion_item_fact() {
    let source = include_str!("../completion.rs");
    for forbidden in [
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
        "fn root_completions",
        "fn saved_path_completions",
        "fn namespace_completions",
        "fn type_completions",
        "fn bare_completions",
    ] {
        assert!(
            !source.contains(forbidden),
            "completion must consume Marrow completion item facts instead of assembling candidates through {forbidden}"
        );
    }
    assert!(
        source.contains("source_completion_fact"),
        "completion must request Marrow's completion item fact"
    );
}

#[test]
fn resource_namespace_does_not_list_identity() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x: Book::|\n",
    );
    assert_eq!(labels, Vec::<String>::new());
}

#[test]
fn used_module_qualified_resource_namespace_does_not_list_identity() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const x: books::Book::|\n",
    );
    assert_eq!(labels, Vec::<String>::new());
}

#[test]
fn fully_qualified_resource_namespace_does_not_list_identity() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x: shelf::books::Book::|\n",
    );
    assert_eq!(labels, Vec::<String>::new());
}

#[test]
fn foreign_bare_enum_namespace_returns_no_items() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x = Status::|\n",
    );
    assert!(
        labels.is_empty(),
        "foreign enum namespace completion should stay closed, got {labels:?}"
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
fn used_module_qualified_enum_namespace_lists_members_from_marrow_fact() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const x = books::Status::|\n",
    );
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert_eq!(labels, ["active", "archived", "retired"]);
    let active = item_named(&items, "active");
    assert_eq!(active.kind, Some(CompletionItemKind::ENUM_MEMBER));
    assert_eq!(active.detail.as_deref(), Some("Status"));
    assert_eq!(documentation_value(active), "Ready for use.");
}

#[test]
fn fully_qualified_enum_namespace_lists_members_from_marrow_fact() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x = shelf::books::Status::|\n",
    );
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert_eq!(labels, ["active", "archived", "retired"]);
    let retired = item_named(&items, "retired");
    assert_eq!(retired.kind, Some(CompletionItemKind::ENUM_MEMBER));
    assert_eq!(retired.detail.as_deref(), Some("Status"));
    assert_eq!(documentation_value(retired), "No longer active.");
}

#[test]
fn expected_enum_value_completion_consumes_marrow_completion_fact_items() {
    let (program, file) = project();

    for (label, source, prefix) in [
        (
            "annotated const initializer",
            "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const state: Status = a|\n",
            "Status",
        ),
        (
            "annotated var initializer",
            "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    var state: Status = a|\n",
            "Status",
        ),
        (
            "qualified annotated const initializer",
            "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const state: books::Status = a|\n",
            "books::Status",
        ),
        (
            "enum return expression",
            "module shelf::app\n\nuse shelf::books\n\npub fn f(): Status\n    return a|\n",
            "Status",
        ),
        (
            "nested enum return expression",
            "module shelf::app\n\nuse shelf::books\n\npub fn f(): Status\n    if true\n        return a|\n    return Status::active\n",
            "Status",
        ),
        (
            "function enum argument",
            "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const state = books::chooseStatus(\"current\", a|)\n",
            "Status",
        ),
        (
            "resource constructor enum field",
            "module shelf::app\n\nuse shelf::books\n\nresource Draft\n    required title: string\n    required state: books::Status\n\npub fn f()\n    const draft = Draft(title: \"draft\", state: a|)\n",
            "Status",
        ),
        (
            "local enum assignment",
            "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    var state: Status = Status::active\n    state = a|\n",
            "Status",
        ),
        (
            "resource field enum assignment",
            "module shelf::app\n\nuse shelf::books\n\nresource Draft\n    required title: string\n    required state: books::Status\n\npub fn f()\n    var draft: Draft\n    draft.state = a|\n",
            "Status",
        ),
    ] {
        let items = complete_items(&program, &file, source);
        let active = item_named(&items, &format!("{prefix}::active"));
        assert_eq!(
            active.kind,
            Some(CompletionItemKind::ENUM_MEMBER),
            "{label}"
        );
        assert_eq!(active.detail.as_deref(), Some("Status"), "{label}");
        assert_eq!(documentation_value(active), "Ready for use.", "{label}");

        let retired = item_named(&items, &format!("{prefix}::archived::retired"));
        assert_eq!(
            retired.kind,
            Some(CompletionItemKind::ENUM_MEMBER),
            "{label}"
        );
        assert_eq!(retired.detail.as_deref(), Some("Status"), "{label}");
        assert_eq!(documentation_value(retired), "No longer active.", "{label}");

        let labels = items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();
        let category_label = format!("{prefix}::archived");
        assert!(
            !labels.contains(&category_label.as_str()),
            "{label}: expected enum completion must not offer category members: {labels:?}"
        );
        assert!(
            !labels.iter().any(|label| label.ends_with("hidden")),
            "{label}: expected enum completion must not leak another enum's members: {labels:?}"
        );
        assert!(
            labels.contains(&"return"),
            "{label}: expected enum completion should remain additive with bare completions"
        );
    }

    let first_arg_items = complete_items(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const state = books::chooseStatus(a|, Status::active)\n",
    );
    let first_arg_labels = first_arg_items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert!(
        !first_arg_labels.contains(&"Status::active"),
        "string arguments must not receive enum value completions: {first_arg_labels:?}"
    );

    let assignment_first_arg_items = complete_items(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    var state: Status = Status::active\n    state = books::chooseStatus(a|, Status::active)\n",
    );
    let assignment_first_arg_labels = assignment_first_arg_items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert!(
        !assignment_first_arg_labels.contains(&"Status::active"),
        "outer assignment expected types must not leak into string call arguments: {assignment_first_arg_labels:?}"
    );

    for (label, source) in [
        (
            "empty string argument in annotated const initializer",
            "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const state: Status = books::chooseStatus(|, Status::active)\n",
        ),
        (
            "empty string argument in assignment value",
            "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    var state: Status = Status::active\n    state = books::chooseStatus(|, Status::active)\n",
        ),
        (
            "empty string argument in return value",
            "module shelf::app\n\nuse shelf::books\n\npub fn f(): Status\n    return books::chooseStatus(|, Status::active)\n",
        ),
    ] {
        let items = complete_items(&program, &file, source);
        let labels = items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();
        assert!(
            !labels.contains(&"Status::active"),
            "{label}: outer expected enum values must not leak into empty string call arguments: {labels:?}"
        );
    }

    for (label, source) in [
        (
            "empty annotated const initializer",
            "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const state: Status = |\n",
        ),
        (
            "empty assignment value",
            "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    var state: Status = Status::active\n    state = |\n",
        ),
        (
            "empty return expression",
            "module shelf::app\n\nuse shelf::books\n\npub fn f(): Status\n    return |\n",
        ),
        (
            "empty resource constructor enum field",
            "module shelf::app\n\nuse shelf::books\n\nresource Draft\n    required title: string\n    required state: books::Status\n\npub fn f()\n    const draft = Draft(state: |)\n",
        ),
    ] {
        let items = complete_items(&program, &file, source);
        assert_eq!(
            item_named(&items, "Status::active").kind,
            Some(CompletionItemKind::ENUM_MEMBER),
            "{label}"
        );
        let labels = items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();
        assert!(
            !labels.contains(&"Status::archived"),
            "{label}: value positions must not offer category members: {labels:?}"
        );
    }

    let constructor_field_name_items = complete_items(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const draft = Draft(st|)\n",
    );
    let constructor_field_name_labels = constructor_field_name_items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert!(
        !constructor_field_name_labels.contains(&"Status::active"),
        "constructor field-name positions must not receive enum value completions: {constructor_field_name_labels:?}"
    );
}

#[test]
fn expected_enum_match_arm_completion_consumes_marrow_completion_fact_items() {
    let (program, file) = project();

    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f(state: Status)\n    match state\n        a|\n            return\n",
    );
    let active = item_named(&items, "active");
    assert_eq!(active.kind, Some(CompletionItemKind::ENUM_MEMBER));
    assert_eq!(active.detail.as_deref(), Some("Status match arm"));
    assert_eq!(documentation_value(active), "Ready for use.");

    let archived = item_named(&items, "archived");
    assert_eq!(archived.kind, Some(CompletionItemKind::ENUM_MEMBER));
    assert_eq!(archived.detail.as_deref(), Some("Status match arm"));

    let retired = item_named(&items, "archived::retired");
    assert_eq!(retired.kind, Some(CompletionItemKind::ENUM_MEMBER));
    assert_eq!(retired.detail.as_deref(), Some("Status match arm"));
    assert_eq!(documentation_value(retired), "No longer active.");

    let labels = items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert!(
        !labels.contains(&"Status::active"),
        "match arm completions are relative to the scrutinee enum: {labels:?}"
    );

    let empty_items = complete_items(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f(state: Status)\n    match state\n        |\n",
    );
    assert_eq!(
        item_named(&empty_items, "active").kind,
        Some(CompletionItemKind::ENUM_MEMBER),
        "empty match arm header should offer relative enum members"
    );
    let empty_labels = empty_items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();
    assert!(
        !empty_labels.contains(&"Status::active"),
        "empty match arm completions stay relative: {empty_labels:?}"
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
fn used_module_function_completion_lists_public_function_from_marrow_fact() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const x = books::|\n",
    );
    let title = item_named(&items, "titleOf");
    assert_eq!(title.kind, Some(CompletionItemKind::FUNCTION));
    assert_eq!(
        title.detail.as_deref(),
        Some("fn titleOf(id: Id(^books)): string")
    );
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert!(!labels.contains(&"privateTitle"), "got {labels:?}");
    assert!(!labels.contains(&"LIMIT"), "got {labels:?}");
}

#[test]
fn used_module_namespace_lists_schema_members_from_marrow_fact() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f()\n    const x = books::|\n",
    );
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Book", "Pair", "Status", "titleOf", "chooseStatus"]
    );

    let book = item_named(&items, "Book");
    assert_eq!(book.kind, Some(CompletionItemKind::STRUCT));
    assert_eq!(book.detail.as_deref(), Some("resource"));

    let status = item_named(&items, "Status");
    assert_eq!(status.kind, Some(CompletionItemKind::ENUM));
    assert_eq!(status.detail.as_deref(), Some("enum"));
    assert!(!labels.contains(&"Secret"), "got {labels:?}");
}

#[test]
fn used_module_namespace_ignores_local_alias_collision_in_sibling_function() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "\
module shelf::app

use shelf::books

pub fn shadow(): int
    const books = 5
    return books

pub fn f()
    const x = books::|
",
    );
    let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Book", "Pair", "Status", "titleOf", "chooseStatus"]
    );
}

#[test]
fn partial_project_module_prefix_returns_no_namespace_items() {
    let (program, file) = project();
    let labels = complete(
        &program,
        &file,
        "module shelf::app\n\npub fn f()\n    const x = shelf::|\n",
    );
    assert_eq!(labels, Vec::<String>::new());
}

#[test]
fn type_position_lists_resources_and_builtin_types() {
    let (program, file) = project();
    let items = complete_items(
        &program,
        &file,
        "module shelf::app\n\nuse shelf::books\n\npub fn f(x: |\n",
    );
    let labels = items
        .iter()
        .map(|item| item.label.clone())
        .collect::<Vec<_>>();
    assert!(
        labels.contains(&"int".to_string()),
        "a builtin type, got {labels:?}"
    );
    assert!(
        labels.contains(&"ErrorCode".to_string()),
        "the error-code scalar spelling, got {labels:?}"
    );
    assert!(
        labels.contains(&"Error".to_string()),
        "the builtin error type, got {labels:?}"
    );
    assert!(
        labels.contains(&"Draft".to_string()),
        "a same-module resource, got {labels:?}"
    );
    assert!(
        labels.contains(&"books::Book".to_string()),
        "an imported resource, got {labels:?}"
    );
    assert!(
        !labels.contains(&"Book".to_string()),
        "a foreign resource must not be offered bare, got {labels:?}"
    );
    assert!(
        labels.contains(&"Id(^books)".to_string()),
        "a store identity, got {labels:?}"
    );
    assert!(
        labels.contains(&"Status".to_string()),
        "a unique visible foreign enum type, got {labels:?}"
    );
    assert!(
        labels.contains(&"books::Status".to_string()),
        "an imported enum type, got {labels:?}"
    );
    assert!(
        labels.contains(&"LocalSecret".to_string()),
        "a same-module private enum type, got {labels:?}"
    );
    assert!(
        !labels.contains(&"Secret".to_string()),
        "a private foreign enum must not be offered bare, got {labels:?}"
    );

    let int = item_named(&items, "int");
    assert_eq!(int.kind, Some(CompletionItemKind::KEYWORD));
    assert_eq!(int.detail.as_deref(), Some("type"));

    let error_code = item_named(&items, "ErrorCode");
    assert_eq!(error_code.kind, Some(CompletionItemKind::KEYWORD));
    assert_eq!(error_code.detail.as_deref(), Some("type"));

    let error = item_named(&items, "Error");
    assert_eq!(error.kind, Some(CompletionItemKind::KEYWORD));
    assert_eq!(error.detail.as_deref(), Some("type"));

    let draft = item_named(&items, "Draft");
    assert_eq!(draft.kind, Some(CompletionItemKind::STRUCT));
    assert_eq!(draft.detail.as_deref(), Some("resource"));
    assert_eq!(documentation_value(draft), "Draft resource docs.");

    let book = item_named(&items, "books::Book");
    assert_eq!(book.kind, Some(CompletionItemKind::STRUCT));
    assert_eq!(book.detail.as_deref(), Some("resource"));
    assert_eq!(documentation_value(book), "Book resource docs.");

    let identity = item_named(&items, "Id(^books)");
    assert_eq!(identity.kind, Some(CompletionItemKind::CLASS));
    assert_eq!(identity.detail.as_deref(), Some("identity of ^books"));
    assert_eq!(documentation_value(identity), "Books saved by id.");

    let status = item_named(&items, "Status");
    assert_eq!(status.kind, Some(CompletionItemKind::ENUM));
    assert_eq!(status.detail.as_deref(), Some("enum"));
    assert_eq!(documentation_value(status), "Lifecycle state.");
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
