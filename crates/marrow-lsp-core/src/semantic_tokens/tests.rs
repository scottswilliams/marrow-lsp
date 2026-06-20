use super::*;
use marrow_check::{ProjectSources, analyze_project, build_binding_index};
use marrow_project::parse_config;
use marrow_syntax::{Token, lex_source, parse_source};

type DecodedToken = (u32, u32, u32, u32, u32);
type DecodedTokens = Vec<DecodedToken>;

/// Decode the delta-encoded tokens back to absolute `(line, character, length,
/// type, modifiers)` tuples so a test can assert on positions directly.
fn absolute(tokens: &[SemanticToken]) -> DecodedTokens {
    let mut line = 0;
    let mut start = 0;
    tokens
        .iter()
        .map(|token| {
            if token.delta_line == 0 {
                start += token.delta_start;
            } else {
                line += token.delta_line;
                start = token.delta_start;
            }
            (
                line,
                start,
                token.length,
                token.token_type,
                token.token_modifiers_bitset,
            )
        })
        .collect()
}

fn legend_index(semantic_type: &SemanticTokenType) -> u32 {
    legend()
        .token_types
        .iter()
        .position(|candidate| candidate == semantic_type)
        .unwrap_or_else(|| panic!("legend should contain {semantic_type:?}")) as u32
}

fn modifier_bit(modifier: &SemanticTokenModifier) -> u32 {
    let index = legend()
        .token_modifiers
        .iter()
        .position(|candidate| candidate == modifier)
        .unwrap_or_else(|| panic!("legend should contain {modifier:?}"));
    1 << index
}

fn decoded_for(source: &str) -> (LineIndex, DecodedTokens) {
    let index = LineIndex::new(source);
    let parsed = parse_source(source);
    let decoded = absolute(&semantic_tokens(&lex_source(source), &parsed, &index));
    (index, decoded)
}

fn decoded_for_checked(source: &str) -> (LineIndex, DecodedTokens) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("m.mw");
    std::fs::write(&file, source).unwrap();

    let config =
        parse_config(r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#).unwrap();
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None, None).unwrap();
    let binding_index = build_binding_index(&snapshot);
    let parsed = snapshot
        .files
        .iter()
        .find(|analyzed| analyzed.path == file)
        .expect("analyzed source file");
    let index = LineIndex::new(source);
    let decoded = absolute(&semantic_tokens_with_project_facts(
        &lex_source(source),
        &parsed.parsed,
        &index,
        Some((&snapshot, &file)),
        Some((&binding_index, &file)),
    ));
    assert!(
        !snapshot.program.modules.is_empty(),
        "checked fixture should provide binding facts"
    );
    (index, decoded)
}

fn decoded_for_checked_file(
    files: &[(&str, &str)],
    active_relative: &str,
) -> (LineIndex, DecodedTokens) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for (relative, source) in files {
        let file = src.join(relative);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(file, source).unwrap();
    }

    let active_source = files
        .iter()
        .find_map(|(relative, source)| (*relative == active_relative).then_some(*source))
        .expect("active source file should be in the fixture");
    let active_file = src.join(active_relative);
    let config =
        parse_config(r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#).unwrap();
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None, None).unwrap();
    let binding_index = build_binding_index(&snapshot);
    let parsed = snapshot
        .files
        .iter()
        .find(|analyzed| analyzed.path == active_file)
        .expect("analyzed active source file");
    let index = LineIndex::new(active_source);
    let decoded = absolute(&semantic_tokens_with_project_facts(
        &lex_source(active_source),
        &parsed.parsed,
        &index,
        Some((&snapshot, &active_file)),
        Some((&binding_index, &active_file)),
    ));
    assert!(
        !snapshot.program.modules.is_empty(),
        "checked fixture should provide binding facts"
    );
    (index, decoded)
}

fn assert_token_type(
    source: &str,
    index: &LineIndex,
    decoded: &[DecodedToken],
    line_text: &str,
    lexeme: &str,
    expected_type: u32,
) {
    let line_start = source
        .find(line_text)
        .unwrap_or_else(|| panic!("source should contain line {line_text:?}"));
    let in_line = line_text
        .find(lexeme)
        .unwrap_or_else(|| panic!("line {line_text:?} should contain {lexeme:?}"));
    let position = index.position(line_start + in_line);
    let length = lexeme.encode_utf16().count() as u32;
    let token = decoded
        .iter()
        .find(|(line, start, len, _, _)| {
            *line == position.line && *start == position.character && *len == length
        })
        .unwrap_or_else(|| {
            panic!(
                "semantic token for {lexeme:?} at {}:{} should exist in {decoded:?}",
                position.line, position.character
            )
        });
    assert_eq!(
        token.3, expected_type,
        "{lexeme:?} on {line_text:?} should have semantic token type {expected_type}"
    );
}

fn assert_token(
    source: &str,
    index: &LineIndex,
    decoded: &[DecodedToken],
    line_text: &str,
    lexeme: &str,
    expected_type: u32,
    expected_modifiers: u32,
) {
    let line_start = source
        .find(line_text)
        .unwrap_or_else(|| panic!("source should contain line {line_text:?}"));
    let in_line = line_text
        .find(lexeme)
        .unwrap_or_else(|| panic!("line {line_text:?} should contain {lexeme:?}"));
    let position = index.position(line_start + in_line);
    let length = lexeme.encode_utf16().count() as u32;
    let token = decoded
        .iter()
        .find(|(line, start, len, _, _)| {
            *line == position.line && *start == position.character && *len == length
        })
        .unwrap_or_else(|| {
            panic!(
                "semantic token for {lexeme:?} at {}:{} should exist in {decoded:?}",
                position.line, position.character
            )
        });
    assert_eq!(
        (token.3, token.4),
        (expected_type, expected_modifiers),
        "{lexeme:?} on {line_text:?} should have semantic token type/modifiers"
    );
}

#[test]
fn legend_lists_standard_lsp_declaration_types() {
    let legend = legend();
    for semantic_type in [
        SemanticTokenType::FUNCTION,
        SemanticTokenType::STRUCT,
        SemanticTokenType::ENUM,
        SemanticTokenType::ENUM_MEMBER,
        SemanticTokenType::PROPERTY,
        SemanticTokenType::PARAMETER,
        SemanticTokenType::NAMESPACE,
    ] {
        assert!(
            legend.token_types.contains(&semantic_type),
            "legend should contain {semantic_type:?}"
        );
    }
}

#[test]
fn the_legend_lists_the_default_library_modifier() {
    assert!(
        legend()
            .token_modifiers
            .contains(&SemanticTokenModifier::DEFAULT_LIBRARY),
        "the legend must advertise the defaultLibrary modifier"
    );
}

#[test]
fn readonly_modifier_is_appended_after_default_library() {
    let legend = legend();
    let default_library = legend
        .token_modifiers
        .iter()
        .position(|modifier| modifier == &SemanticTokenModifier::DEFAULT_LIBRARY)
        .expect("legend should contain defaultLibrary");
    let readonly = legend
        .token_modifiers
        .iter()
        .position(|modifier| modifier == &SemanticTokenModifier::READONLY)
        .expect("legend should contain readonly");

    assert_eq!(
        readonly,
        default_library + 1,
        "readonly should be appended after existing modifiers so prior bits stay stable"
    );
}

#[test]
fn boolean_literal_token_is_appended_after_parameter() {
    let boolean_literal = SemanticTokenType::new("booleanLiteral");

    assert_eq!(
        legend_index(&boolean_literal),
        legend_index(&SemanticTokenType::PARAMETER) + 1,
        "booleanLiteral should be appended after parameter so existing token indices stay stable"
    );
}

#[test]
fn true_and_false_color_as_boolean_literals() {
    let source = "\
module m

fn f(): bool
    return true and false
";
    let (index, decoded) = decoded_for(source);
    let boolean_literal = SemanticTokenType::new("booleanLiteral");
    let boolean_literal = legend_index(&boolean_literal);

    assert_token(
        source,
        &index,
        &decoded,
        "    return true and false",
        "true",
        boolean_literal,
        0,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    return true and false",
        "false",
        boolean_literal,
        0,
    );
}

#[test]
fn coalesce_colors_as_operator() {
    let source = "\
module m

fn f(a: string, b: string): string
    return a ?? b
";
    let (index, decoded) = decoded_for(source);

    assert_token(
        source,
        &index,
        &decoded,
        "    return a ?? b",
        "??",
        legend_index(&SemanticTokenType::OPERATOR),
        0,
    );
}

#[test]
fn std_namespaces_are_default_library() {
    let source = "\
module m

fn f()
    const len = std::text::length(\"abc\")
";
    let (index, decoded) = decoded_for(source);
    let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);

    assert_token(
        source,
        &index,
        &decoded,
        "    const len = std::text::length(\"abc\")",
        "std",
        legend_index(&SemanticTokenType::NAMESPACE),
        default_library,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    const len = std::text::length(\"abc\")",
        "text",
        legend_index(&SemanticTokenType::NAMESPACE),
        default_library,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    const len = std::text::length(\"abc\")",
        "length",
        legend_index(&SemanticTokenType::FUNCTION),
        default_library,
    );
}

#[test]
fn declaration_identifiers_use_role_specific_token_types() {
    let source = "\
module shelf::catalog
use shared::imports

resource Book
    required title: string
    notes(note_id: string)
        text: string
    tags(pos: int): string

store ^books(id: int): Book
    index byTitle(title, id)

pub fn paint(book_id: int, label: string): int
    return book_id

pub enum Genre
    Fiction
        Literary
    Nonfiction
";
    let (index, decoded) = decoded_for(source);

    let namespace = legend_index(&SemanticTokenType::NAMESPACE);
    assert_token_type(
        source,
        &index,
        &decoded,
        "module shelf::catalog",
        "shelf",
        namespace,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "module shelf::catalog",
        "catalog",
        namespace,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "use shared::imports",
        "shared",
        namespace,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "use shared::imports",
        "imports",
        namespace,
    );

    assert_token_type(
        source,
        &index,
        &decoded,
        "resource Book",
        "Book",
        legend_index(&SemanticTokenType::STRUCT),
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "pub fn paint(book_id: int, label: string): int",
        "paint",
        legend_index(&SemanticTokenType::FUNCTION),
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "pub enum Genre",
        "Genre",
        legend_index(&SemanticTokenType::ENUM),
    );

    let enum_member = legend_index(&SemanticTokenType::ENUM_MEMBER);
    assert_token_type(
        source,
        &index,
        &decoded,
        "    Fiction",
        "Fiction",
        enum_member,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "        Literary",
        "Literary",
        enum_member,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    Nonfiction",
        "Nonfiction",
        enum_member,
    );

    let property = legend_index(&SemanticTokenType::PROPERTY);
    assert_token_type(
        source,
        &index,
        &decoded,
        "    required title: string",
        "title",
        property,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    notes(note_id: string)",
        "notes",
        property,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "        text: string",
        "text",
        property,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    tags(pos: int): string",
        "tags",
        property,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    index byTitle(title, id)",
        "byTitle",
        property,
    );

    let parameter = legend_index(&SemanticTokenType::PARAMETER);
    assert_token_type(
        source,
        &index,
        &decoded,
        "store ^books(id: int): Book",
        "id",
        parameter,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    notes(note_id: string)",
        "note_id",
        parameter,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    tags(pos: int): string",
        "pos",
        parameter,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    index byTitle(title, id)",
        "title",
        parameter,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    index byTitle(title, id)",
        "id",
        parameter,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "pub fn paint(book_id: int, label: string): int",
        "book_id",
        parameter,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "pub fn paint(book_id: int, label: string): int",
        "label",
        parameter,
    );
}

#[test]
fn surface_declarations_color_names() {
    let source = "\
module shelf

surface Books from ^books
    fields title
";
    let (index, decoded) = decoded_for(source);

    assert_token_type(
        source,
        &index,
        &decoded,
        "surface Books from ^books",
        "Books",
        legend_index(&SemanticTokenType::STRUCT),
    );
}

#[test]
fn module_const_declarations_are_readonly_variables() {
    let source = "\
module m

const LIMIT: int = 10

fn f(): int
    const local_value = LIMIT
    return local_value
";
    let (index, decoded) = decoded_for(source);

    assert_token(
        source,
        &index,
        &decoded,
        "const LIMIT: int = 10",
        "LIMIT",
        legend_index(&SemanticTokenType::VARIABLE),
        modifier_bit(&SemanticTokenModifier::READONLY),
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    const local_value = LIMIT",
        "local_value",
        legend_index(&SemanticTokenType::VARIABLE),
        0,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    const local_value = LIMIT",
        "LIMIT",
        legend_index(&SemanticTokenType::VARIABLE),
        0,
    );
}

#[test]
fn module_const_references_are_readonly_variables_from_binding_facts() {
    let source = "\
module m

const LIMIT: int = 10

fn f(): int
    const local_value = LIMIT
    var mutable_value = local_value
    return mutable_value
";
    let (index, decoded) = decoded_for_checked(source);
    let variable = legend_index(&SemanticTokenType::VARIABLE);
    let readonly = modifier_bit(&SemanticTokenModifier::READONLY);

    assert_token(
        source,
        &index,
        &decoded,
        "    const local_value = LIMIT",
        "LIMIT",
        variable,
        readonly,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    var mutable_value = local_value",
        "local_value",
        variable,
        0,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    return mutable_value",
        "mutable_value",
        variable,
        0,
    );
}

#[test]
fn shadowing_local_const_references_do_not_inherit_module_const_readonly() {
    let source = "\
module m

const LIMIT: int = 10

fn f(): int
    const LIMIT: int = 1
    return LIMIT
";
    let (index, decoded) = decoded_for_checked(source);
    let variable = legend_index(&SemanticTokenType::VARIABLE);
    let readonly = modifier_bit(&SemanticTokenModifier::READONLY);

    assert_token(
        source,
        &index,
        &decoded,
        "const LIMIT: int = 10",
        "LIMIT",
        variable,
        readonly,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    const LIMIT: int = 1",
        "LIMIT",
        variable,
        0,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    return LIMIT",
        "LIMIT",
        variable,
        0,
    );
}

#[test]
fn saved_root_sigil_and_name_are_marked_distinctly_from_a_variable() {
    // `^books` reads/writes saved data; `books` is just a local. They must not
    // carry the same token type.
    let source = "module a\n\npub fn f()\n    const books: int = 1\n    delete ^books(books)\n";
    let index = LineIndex::new(source);
    let parsed = parse_source(source);
    let tokens = semantic_tokens(&lex_source(source), &parsed, &index);
    let decoded = absolute(&tokens);

    // The saved-root tokens carry the dedicated type and the modification bit.
    let saved: Vec<_> = decoded
        .iter()
        .filter(|(_, _, _, ty, _)| *ty == TYPE_SAVED_ROOT)
        .collect();
    assert!(
        !saved.is_empty(),
        "the `^books` saved root should produce saved-root tokens"
    );
    assert!(
        saved.iter().all(|(_, _, _, _, m)| *m == MOD_MODIFICATION),
        "saved-root tokens carry the modification modifier"
    );

    // A plain variable token never uses the saved-root type.
    assert!(
        decoded
            .iter()
            .any(|(_, _, _, ty, m)| *ty == TYPE_VARIABLE && *m == 0),
        "the local `books` should be a plain variable token"
    );
}

#[test]
fn keywords_types_numbers_strings_and_comments_are_classified() {
    let source = "module a\n\nconst N: int = 42 ; a count\n";
    let index = LineIndex::new(source);
    let parsed = parse_source(source);
    let decoded = absolute(&semantic_tokens(&lex_source(source), &parsed, &index));
    let types: std::collections::HashSet<u32> =
        decoded.iter().map(|(_, _, _, ty, _)| *ty).collect();
    assert!(
        types.contains(&TYPE_KEYWORD),
        "`module`/`const` are keywords"
    );
    assert!(types.contains(&TYPE_TYPE), "`int` is a type keyword");
    assert!(types.contains(&TYPE_NUMBER), "`42` is a number");
    assert!(
        types.contains(&TYPE_COMMENT),
        "the `;` comment is a comment"
    );
}

#[test]
fn namespace_separator_is_its_own_type() {
    let source = "module a\n\npub fn f()\n    std::clock::now()\n";
    let index = LineIndex::new(source);
    let parsed = parse_source(source);
    let decoded = absolute(&semantic_tokens(&lex_source(source), &parsed, &index));
    assert!(
        decoded.iter().any(|(_, _, _, ty, _)| *ty == TYPE_NAMESPACE),
        "the `::` separator colors as a namespace operator"
    );
}

#[test]
fn resolved_reference_uses_take_their_symbol_roles() {
    let source = "\
module m

resource Book
    required title: string
    tags(pos: int): string

store ^books(id: int): Book
    index byTitle(title)

enum Status
    active
    archived

fn helper(id: int): int
    return id

fn paint(id: int, title: string): int
    const book = Book(title: title)
    const status = Status::archived
    const found = ^books(id).title ?? \"\"
    const tag = ^books(id).tags(1) ?? \"\"
    const lookup = ^books.byTitle(title)
    return helper(id)
";
    let (index, decoded) = decoded_for_checked(source);

    assert_token_type(
        source,
        &index,
        &decoded,
        "    return helper(id)",
        "id",
        legend_index(&SemanticTokenType::PARAMETER),
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    return helper(id)",
        "helper",
        legend_index(&SemanticTokenType::FUNCTION),
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    const book = Book(title: title)",
        "Book",
        legend_index(&SemanticTokenType::STRUCT),
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    const status = Status::archived",
        "archived",
        legend_index(&SemanticTokenType::ENUM_MEMBER),
    );

    let property = legend_index(&SemanticTokenType::PROPERTY);
    assert_token_type(
        source,
        &index,
        &decoded,
        "    const found = ^books(id).title ?? \"\"",
        "title",
        property,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    const tag = ^books(id).tags(1) ?? \"\"",
        "tags",
        property,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    const lookup = ^books.byTitle(title)",
        "byTitle",
        property,
    );
}

#[test]
fn declaration_header_type_tokens_are_not_recolored_by_definition_spans() {
    let source = "\
module m

resource Book
    required title: string
    tags(pos: int): string

store ^books(id: int): Book

fn helper(id: int): int
    return id
";
    let (index, decoded) = decoded_for_checked(source);
    let ty = legend_index(&SemanticTokenType::TYPE);

    assert_token_type(
        source,
        &index,
        &decoded,
        "store ^books(id: int): Book",
        "int",
        ty,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    required title: string",
        "string",
        ty,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "    tags(pos: int): string",
        "string",
        ty,
    );
    assert_token_type(
        source,
        &index,
        &decoded,
        "fn helper(id: int): int",
        "int",
        ty,
    );
}

#[test]
fn enum_type_annotation_references_color_as_enums() {
    let source = "\
module m

enum Status
    active
    archived

fn f()
    const current: Status = Status::active
";
    let (index, decoded) = decoded_for_checked(source);

    assert_token_type(
        source,
        &index,
        &decoded,
        "    const current: Status = Status::active",
        "Status",
        legend_index(&SemanticTokenType::ENUM),
    );
}

#[test]
fn checked_qualified_enum_type_prefix_is_namespace_while_leaf_stays_enum() {
    let status_source = "\
module a::b

pub enum Status
    active
    archived
";
    let app_source = "\
module app

use a::b

fn f(): b::Status
    return b::Status::active
";
    let (index, decoded) = decoded_for_checked_file(
        &[("a/b.mw", status_source), ("app.mw", app_source)],
        "app.mw",
    );

    assert_token(
        app_source,
        &index,
        &decoded,
        "fn f(): b::Status",
        "b",
        legend_index(&SemanticTokenType::NAMESPACE),
        0,
    );
    assert_token(
        app_source,
        &index,
        &decoded,
        "fn f(): b::Status",
        "Status",
        legend_index(&SemanticTokenType::ENUM),
        0,
    );
}

#[test]
fn checked_qualified_enum_member_prefixes_are_namespaces_while_leaves_keep_roles() {
    let status_source = "\
module a::b

pub enum Status
    active
    archived
";
    let app_source = "\
module app

use a::b

fn f(): b::Status
    return b::Status::active
";
    let (index, decoded) = decoded_for_checked_file(
        &[("a/b.mw", status_source), ("app.mw", app_source)],
        "app.mw",
    );

    assert_token(
        app_source,
        &index,
        &decoded,
        "    return b::Status::active",
        "b",
        legend_index(&SemanticTokenType::NAMESPACE),
        0,
    );
    assert_token(
        app_source,
        &index,
        &decoded,
        "    return b::Status::active",
        "Status",
        legend_index(&SemanticTokenType::ENUM),
        0,
    );
    assert_token(
        app_source,
        &index,
        &decoded,
        "    return b::Status::active",
        "active",
        legend_index(&SemanticTokenType::ENUM_MEMBER),
        0,
    );
}

#[test]
fn checked_fully_qualified_enum_type_prefixes_are_namespaces_while_leaf_stays_enum() {
    let status_source = "\
module a::b::c

pub enum Status
    active
    archived
";
    let app_source = "\
module app

fn f(): a::b::c::Status
    return a::b::c::Status::active
";
    let (index, decoded) = decoded_for_checked_file(
        &[("a/b/c.mw", status_source), ("app.mw", app_source)],
        "app.mw",
    );
    let namespace = legend_index(&SemanticTokenType::NAMESPACE);

    for lexeme in ["a", "b", "c"] {
        assert_token(
            app_source,
            &index,
            &decoded,
            "fn f(): a::b::c::Status",
            lexeme,
            namespace,
            0,
        );
    }
    assert_token(
        app_source,
        &index,
        &decoded,
        "fn f(): a::b::c::Status",
        "Status",
        legend_index(&SemanticTokenType::ENUM),
        0,
    );
}

#[test]
fn known_default_library_calls_carry_default_library_modifier() {
    let source = "\
module m

resource Book
    tags(pos: int): string

store ^books(id: int): Book

fn f(): bool
    const clock_value = std::clock::now()
    const has_book = exists(^books(1))
    const ids = keys(^books)
    const added = append(^books(1).tags, \"tag\")
    const next = nextId(^books)
    const c1 = int(\"1\")
    const c2 = decimal(\"1.5\")
    const c3 = string(1)
    const c4 = bool(\"true\")
    return has_book
";
    let (index, decoded) = decoded_for_checked(source);
    let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);
    let function = legend_index(&SemanticTokenType::FUNCTION);
    let ty = legend_index(&SemanticTokenType::TYPE);

    for (line, lexeme, expected_type) in [
        ("    const clock_value = std::clock::now()", "now", function),
        ("    const has_book = exists(^books(1))", "exists", function),
        ("    const ids = keys(^books)", "keys", function),
        (
            "    const added = append(^books(1).tags, \"tag\")",
            "append",
            function,
        ),
        ("    const next = nextId(^books)", "nextId", function),
        ("    const c1 = int(\"1\")", "int", ty),
        ("    const c2 = decimal(\"1.5\")", "decimal", ty),
        ("    const c3 = string(1)", "string", ty),
        ("    const c4 = bool(\"true\")", "bool", ty),
    ] {
        assert_token(
            source,
            &index,
            &decoded,
            line,
            lexeme,
            expected_type,
            default_library,
        );
    }
}

#[test]
fn standard_library_calls_use_the_shared_std_table() {
    let source = "\
module m

fn f()
    const text_len = std::text::length(\"abc\")
    std::assert::isTrue(true)
    const unknown_value = std::text::missing(\"abc\")
";
    let (index, decoded) = decoded_for_checked(source);
    let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);
    let function = legend_index(&SemanticTokenType::FUNCTION);
    let namespace = legend_index(&SemanticTokenType::NAMESPACE);

    assert_token(
        source,
        &index,
        &decoded,
        "    std::assert::isTrue(true)",
        "std",
        namespace,
        default_library,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    std::assert::isTrue(true)",
        "assert",
        namespace,
        default_library,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    const text_len = std::text::length(\"abc\")",
        "length",
        function,
        default_library,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    std::assert::isTrue(true)",
        "isTrue",
        function,
        default_library,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    const unknown_value = std::text::missing(\"abc\")",
        "missing",
        legend_index(&SemanticTokenType::VARIABLE),
        0,
    );
}

#[test]
fn imported_standard_library_module_calls_use_marrow_callable_facts() {
    let source = "\
module m
use std::text

fn f()
    const len_value = text::length(\"abc\")
";
    let (index, decoded) = decoded_for_checked(source);
    let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);

    assert_token(
        source,
        &index,
        &decoded,
        "    const len_value = text::length(\"abc\")",
        "text",
        legend_index(&SemanticTokenType::NAMESPACE),
        default_library,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    const len_value = text::length(\"abc\")",
        "length",
        legend_index(&SemanticTokenType::FUNCTION),
        default_library,
    );
}

#[test]
fn semantic_tokens_have_no_local_intrinsic_callable_model() {
    let source = include_str!("builtins.rs");
    for forbidden in [
        "marrow_schema::stdlib",
        "language_facts::bare_builtin_kind",
        "BareBuiltinKind",
        "active_callable_context",
    ] {
        assert!(
            !source.contains(forbidden),
            "semantic tokens must consume marrow_check callable facts instead of {forbidden}"
        );
    }
}

#[test]
fn checked_qualified_call_prefixes_are_namespaces_while_leaf_keeps_role() {
    let source = "\
module m

pub fn helper(): int
    return 1

fn f(): int
    return m::helper()
";
    let (index, decoded) = decoded_for_checked(source);

    assert_token(
        source,
        &index,
        &decoded,
        "    return m::helper()",
        "m",
        legend_index(&SemanticTokenType::NAMESPACE),
        0,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "    return m::helper()",
        "helper",
        legend_index(&SemanticTokenType::FUNCTION),
        0,
    );
}

#[test]
fn checked_qualified_resource_constructor_prefix_is_namespace_while_leaf_stays_struct() {
    let state_source = "\
module shelf::state

resource Book
    required title: string

store ^state_books(id: int): Book
";
    let app_source = "\
module shelf::app

use shelf::state

resource Book
    required label: string

store ^app_books(code: string): Book

pub fn make()
    const book = state::Book(title: \"x\")
";
    let (index, decoded) = decoded_for_checked_file(
        &[
            ("shelf/state.mw", state_source),
            ("shelf/app.mw", app_source),
        ],
        "shelf/app.mw",
    );

    assert_token(
        app_source,
        &index,
        &decoded,
        "    const book = state::Book(title: \"x\")",
        "state",
        legend_index(&SemanticTokenType::NAMESPACE),
        0,
    );
    assert_token(
        app_source,
        &index,
        &decoded,
        "    const book = state::Book(title: \"x\")",
        "Book",
        legend_index(&SemanticTokenType::STRUCT),
        0,
    );
}

#[test]
fn checked_canonical_identity_type_annotation_colors_id_as_struct() {
    let source = "\
module m

resource Author
    name: string

store ^authors(id: int): Author

pub fn f(
    id: Id(^authors),
): Id(^authors)
    return id
";
    let (index, decoded) = decoded_for_checked(source);

    assert_token(
        source,
        &index,
        &decoded,
        "    id: Id(^authors),",
        "Id",
        legend_index(&SemanticTokenType::STRUCT),
        0,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "): Id(^authors)",
        "Id",
        legend_index(&SemanticTokenType::STRUCT),
        0,
    );
}

#[test]
fn evolve_transform_type_annotations_color_identity_id_as_struct() {
    let source = "\
module m

evolve
    transform ^books
        const id: Id(^books) = 1
";
    let (index, decoded) = decoded_for(source);

    assert_token(
        source,
        &index,
        &decoded,
        "        const id: Id(^books) = 1",
        "Id",
        legend_index(&SemanticTokenType::STRUCT),
        0,
    );
}

#[test]
fn evolve_step_words_color_as_keywords_inside_evolve_blocks() {
    let source = "\
module m

evolve
    rename Book.title -> Book.name
    default Book.name = \"untitled\"
    retire Book.title
    transform ^books
        const id: Id(^books) = 1
";
    let (index, decoded) = decoded_for(source);
    let keyword = legend_index(&SemanticTokenType::KEYWORD);

    for (line, word) in [
        ("    rename Book.title -> Book.name", "rename"),
        ("    default Book.name = \"untitled\"", "default"),
        ("    retire Book.title", "retire"),
        ("    transform ^books", "transform"),
    ] {
        assert_token_type(source, &index, &decoded, line, word, keyword);
    }
}

#[test]
fn checked_nested_canonical_identity_type_annotations_color_id_as_struct() {
    let source = "\
module m

resource Author
    name: string

store ^authors(id: int): Author

pub fn f(
    ids: sequence[Id(^authors)],
): sequence[Id(^authors)]
    return ids
";
    let (index, decoded) = decoded_for_checked(source);
    let strukt = legend_index(&SemanticTokenType::STRUCT);

    assert_token(
        source,
        &index,
        &decoded,
        "    ids: sequence[Id(^authors)],",
        "Id",
        strukt,
        0,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "): sequence[Id(^authors)]",
        "Id",
        strukt,
        0,
    );
}

#[test]
fn local_canonical_identity_type_annotations_color_id_as_struct() {
    let source = "\
module m

resource Author
    name: string

store ^authors(id: int): Author

fn f(): Id(^authors)
    const direct: Id(^authors) = nextId(^authors)
    var later: Id(^authors) = direct
    return direct
";
    let (index, decoded) = decoded_for_checked(source);
    let strukt = legend_index(&SemanticTokenType::STRUCT);

    for line in [
        "    const direct: Id(^authors) = nextId(^authors)",
        "    var later: Id(^authors) = direct",
    ] {
        assert_token(source, &index, &decoded, line, "Id", strukt, 0);
    }
}

#[test]
fn longer_canonical_identity_type_paths_are_not_colored_as_store_identity() {
    let source = "\
module m

resource Author
    name: string

store ^authors(id: int): Author

fn bad_extra(x: Id(^authors)::Extra)
    return
";
    let (index, decoded) = decoded_for(source);
    let keyword = legend_index(&SemanticTokenType::KEYWORD);

    assert_token(
        source,
        &index,
        &decoded,
        "fn bad_extra(x: Id(^authors)::Extra)",
        "Id",
        keyword,
        0,
    );
}

#[test]
fn std_library_call_must_start_at_callee_root() {
    let source = "\
module m

fn f()
    const text_len = foo::std::text::length(\"abc\")
";
    let (index, decoded) = decoded_for(source);

    assert_token(
        source,
        &index,
        &decoded,
        "    const text_len = foo::std::text::length(\"abc\")",
        "length",
        legend_index(&SemanticTokenType::VARIABLE),
        0,
    );
}

#[test]
fn bare_builtin_call_must_not_be_qualified_leaf() {
    let source = "\
module m

fn f()
    const found = foo::exists()
";
    let (index, decoded) = decoded_for(source);

    assert_token(
        source,
        &index,
        &decoded,
        "    const found = foo::exists()",
        "exists",
        legend_index(&SemanticTokenType::VARIABLE),
        0,
    );
}

#[test]
fn checker_single_name_builtins_are_default_library_calls() {
    let source = "\
module m

resource Book
    required title: string

store ^books(id: int): Book

fn f()
    const items = keys(^books)
    const rev_items = reversed(items)
    const after = next(items)
    const before = prev(items)
";
    let (index, decoded) = decoded_for(source);
    let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);
    let function = legend_index(&SemanticTokenType::FUNCTION);

    for (line, lexeme) in [
        ("    const rev_items = reversed(items)", "reversed"),
        ("    const after = next(items)", "next"),
        ("    const before = prev(items)", "prev"),
    ] {
        assert_token(
            source,
            &index,
            &decoded,
            line,
            lexeme,
            function,
            default_library,
        );
    }
}

#[test]
fn builtin_call_leaf_wins_over_user_function_binding_reference() {
    let source = "\
module m

resource Book
    required title: string

store ^books(id: int): Book

fn exists(value: unknown): bool
    return false

fn f(): bool
    return exists(^books(1))
";
    let (index, decoded) = decoded_for_checked(source);
    let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);

    assert_token(
        source,
        &index,
        &decoded,
        "    return exists(^books(1))",
        "exists",
        legend_index(&SemanticTokenType::FUNCTION),
        default_library,
    );
    assert_token(
        source,
        &index,
        &decoded,
        "fn exists(value: unknown): bool",
        "exists",
        legend_index(&SemanticTokenType::FUNCTION),
        0,
    );
}

#[test]
fn scalar_conversion_calls_use_the_shared_scalar_names() {
    let source = "\
module m

fn f()
    const b1 = bytes(\"abc\")
    const d1 = date(\"2026-05-30\")
    const i1 = instant(\"2026-05-30T00:00:00+00:00\")
    const span1 = duration(\"1.hour\")
";
    let (index, decoded) = decoded_for_checked(source);
    let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);
    let ty = legend_index(&SemanticTokenType::TYPE);

    for (line, lexeme) in [
        ("    const b1 = bytes(\"abc\")", "bytes"),
        ("    const d1 = date(\"2026-05-30\")", "date"),
        (
            "    const i1 = instant(\"2026-05-30T00:00:00+00:00\")",
            "instant",
        ),
        ("    const span1 = duration(\"1.hour\")", "duration"),
    ] {
        assert_token(source, &index, &decoded, line, lexeme, ty, default_library);
    }
}

#[test]
fn word_operators_color_as_operators() {
    let source = "\
module m

fn f(a: bool, b: bool, value: unknown): bool
    return not a or a and b or value is string
";
    let (index, decoded) = decoded_for(source);
    let operator = legend_index(&SemanticTokenType::OPERATOR);

    for lexeme in ["not", "or", "and", "is"] {
        assert_token_type(
            source,
            &index,
            &decoded,
            "    return not a or a and b or value is string",
            lexeme,
            operator,
        );
    }
}

#[test]
fn duration_literals_color_as_numbers() {
    let source = "module m\n\nconst WAIT: duration = 1.hour\n";
    let (index, decoded) = decoded_for(source);
    assert_token_type(
        source,
        &index,
        &decoded,
        "const WAIT: duration = 1.hour",
        "1.hour",
        legend_index(&SemanticTokenType::NUMBER),
    );
}

#[test]
fn multi_line_tokens_are_clamped_to_the_first_line_remainder() {
    let source = "let value = \"first\nsecond\"";
    let token = Token {
        kind: marrow_syntax::TokenKind::String,
        span: marrow_syntax::SourceSpan {
            start_byte: source.find('"').unwrap(),
            end_byte: source.len(),
            line: 0,
            column: 12,
        },
    };
    let index = LineIndex::new(source);
    let mut tokens = Vec::new();
    let mut line = 0;
    let mut start = 0;

    push(
        &mut tokens,
        &token,
        TYPE_STRING,
        0,
        &index,
        &mut line,
        &mut start,
    );

    let decoded = absolute(&tokens);
    assert_eq!(decoded[0].2, 6, "token covers `\"first` only");
}

#[test]
fn the_legend_lists_the_saved_root_type() {
    let legend = legend();
    assert!(
        legend.token_types.contains(&MARROW_SAVED_ROOT),
        "the legend must advertise the custom saved-root type"
    );
    assert_eq!(legend.token_types.len(), TOKEN_TYPES.len());
    assert_eq!(legend.token_modifiers.len(), TOKEN_MODIFIERS.len());
}
