use super::*;
use crate::positions::LineIndex;
use lsp_types::{Range, Url};
use marrow_check::{AnalysisSnapshot, ProjectSources, analyze_project, build_binding_index};
use marrow_project::parse_config;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A file-index map built from in-memory text, so a test can resolve spans
/// without touching disk. Mirrors how the server threads open buffers' indices.
struct Indices(HashMap<PathBuf, LineIndex>);

impl FileIndex for Indices {
    fn index_for(&self, file: &Path) -> Option<&LineIndex> {
        self.0.get(file)
    }
}

/// Analyze a project on disk and return the snapshot, module file paths, and
/// file indices over the source so navigation maps spans.
fn analyze_files(files: &[(&str, &str)]) -> (AnalysisSnapshot, Vec<PathBuf>, Indices) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut paths = Vec::new();
    let mut indices = HashMap::new();
    for (relative, source) in files {
        let file = src.join(relative);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&file, source).unwrap();
        indices.insert(file.clone(), LineIndex::new((*source).to_string()));
        paths.push(file);
    }
    let config =
        parse_config(r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#).unwrap();
    let snapshot = analyze_project(root, &config, &ProjectSources::new(), None).unwrap();
    std::mem::forget(dir);
    (snapshot, paths, Indices(indices))
}

/// Analyze a one-file project on disk and return the snapshot, the module file
/// path, and a file-index map over the source so navigation maps spans.
fn analyze(source: &str) -> (AnalysisSnapshot, PathBuf, Indices) {
    let (snapshot, mut paths, indices) = analyze_files(&[("a.mw", source)]);
    let file = paths.pop().unwrap();
    (snapshot, file, indices)
}

fn offset_of(source: &str, needle: &str) -> usize {
    source.find(needle).expect("needle present")
}

/// The 0-based line/character of a byte offset, for asserting on a location.
fn line_col(source: &str, offset: usize) -> (u32, u32) {
    let index = LineIndex::new(source.to_string());
    let position = index.position(offset);
    (position.line, position.character)
}

fn range_text<'a>(source: &'a str, index: &LineIndex, range: Range) -> &'a str {
    let start = index.offset(crate::positions::Position {
        line: range.start.line,
        character: range.start.character,
    });
    let end = index.offset(crate::positions::Position {
        line: range.end.line,
        character: range.end.character,
    });
    &source[start..end]
}

fn range_for(source: &str, start: usize, end: usize) -> Range {
    LineIndex::new(source.to_string()).range(start, end)
}

#[test]
fn definition_of_a_local_use_jumps_to_its_binding() {
    let source = "module a\n\npub fn f(): int\n    const n: int = 1\n    return n\n";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);

    // Cursor on the `n` in `return n`.
    let use_offset = offset_of(source, "return n") + "return ".len();
    let location = definition(&snapshot, &index, &indices, &file, use_offset)
        .expect("a definition for the local use");

    // It lands on the `n` in `const n`, the binding site.
    let def_offset = offset_of(source, "const n") + "const ".len();
    let (line, character) = line_col(source, def_offset);
    assert_eq!(location.range.start.line, line);
    assert_eq!(location.range.start.character, character);
}

#[test]
fn definition_from_saved_root_use_jumps_to_store_declaration() {
    let source = "\
module a

resource Book
    required title: string

store ^books(id: int): Book

pub fn f(): string
    return ^books(1).title ?? \"\"
";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);

    let use_offset = offset_of(source, "^books(1)");
    let location = definition(&snapshot, &index, &indices, &file, use_offset)
        .expect("saved root use resolves through catalog facts");
    let line_index = indices.0.get(&file).unwrap();

    assert_eq!(range_text(source, line_index, location.range), "^books");
    let declaration = offset_of(source, "store ^books") + "store ".len();
    assert_eq!(
        location.range,
        range_for(source, declaration, declaration + "^books".len())
    );
}

#[test]
fn definition_from_saved_member_use_jumps_to_resource_member() {
    let source = "\
module a

resource Book
    required title: string

store ^books(id: int): Book

pub fn f(): string
    const title: string = \"fallback\"
    const draft = Book(title: title)
    return ^books(1).title ?? draft.title
";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);

    let use_offset = offset_of(source, ".title ??") + 1;
    let location = definition(&snapshot, &index, &indices, &file, use_offset)
        .expect("saved member use resolves through catalog facts");
    let line_index = indices.0.get(&file).unwrap();

    assert_eq!(range_text(source, line_index, location.range), "title");
    let declaration = offset_of(source, "required title") + "required ".len();
    assert_eq!(
        location.range,
        range_for(source, declaration, declaration + "title".len()),
        "saved member navigation must land on the member declaration, not the same-named local or constructor argument"
    );
}

#[test]
fn definition_from_store_index_use_jumps_to_index_declaration() {
    let source = "\
module a

resource Book
    required title: string
    required shelf: string

store ^books(id: int): Book
    index byShelf(shelf, id)

pub fn f(): int
    return count(^books.byShelf(\"fiction\"))
";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);

    let use_offset = offset_of(source, ".byShelf") + 1;
    let location = definition(&snapshot, &index, &indices, &file, use_offset)
        .expect("store index use resolves through catalog facts");
    let line_index = indices.0.get(&file).unwrap();

    assert_eq!(range_text(source, line_index, location.range), "byShelf");
    let declaration = offset_of(source, "index byShelf") + "index ".len();
    assert_eq!(
        location.range,
        range_for(source, declaration, declaration + "byShelf".len())
    );
}

#[test]
fn references_from_saved_root_use_honor_include_declaration() {
    let source = "\
module a

resource Book
    required title: string

store ^books(id: int): Book

pub fn f(): string
    return ^books(1).title ?? \"\"

pub fn g(): string
    return ^books(2).title ?? \"\"
";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);

    let use_offset = offset_of(source, "^books(1)");
    let with = references(&snapshot, &index, &indices, &file, use_offset, true)
        .expect("saved root references resolve through catalog facts");
    let without = references(&snapshot, &index, &indices, &file, use_offset, false)
        .expect("saved root references resolve through catalog facts");
    let line_index = indices.0.get(&file).unwrap();
    let with_texts: Vec<&str> = with
        .iter()
        .map(|location| range_text(source, line_index, location.range))
        .collect();
    let without_texts: Vec<&str> = without
        .iter()
        .map(|location| range_text(source, line_index, location.range))
        .collect();

    assert_eq!(with_texts, vec!["^books", "^books", "^books"]);
    assert_eq!(without_texts, vec!["^books", "^books"]);

    let declaration = offset_of(source, "store ^books") + "store ".len();
    let declaration_range = range_for(source, declaration, declaration + "^books".len());
    assert!(
        with.iter()
            .any(|location| location.range == declaration_range),
        "include_declaration=true should include the store declaration"
    );
    assert!(
        without
            .iter()
            .all(|location| location.range != declaration_range),
        "include_declaration=false should exclude the store declaration"
    );
}

#[test]
fn references_from_saved_member_use_honor_exact_use_spans_and_include_declaration() {
    let source = "\
module a

resource Book
    required title: string

store ^books(id: int): Book

pub fn f(): string
    return ^books(1).title ?? \"\"

pub fn g(): string
    return ^books(2).title ?? \"\"
";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);
    let line_index = indices.0.get(&file).unwrap();

    let use_offset = offset_of(source, ".title ??") + 1;
    let with = references(&snapshot, &index, &indices, &file, use_offset, true)
        .expect("saved member references resolve through catalog facts");
    let without = references(&snapshot, &index, &indices, &file, use_offset, false)
        .expect("saved member references resolve through catalog facts");
    let with_texts: Vec<&str> = with
        .iter()
        .map(|location| range_text(source, line_index, location.range))
        .collect();
    let without_texts: Vec<&str> = without
        .iter()
        .map(|location| range_text(source, line_index, location.range))
        .collect();

    assert_eq!(with_texts, vec!["title", "title", "title"]);
    assert_eq!(without_texts, vec!["title", "title"]);

    let declaration = offset_of(source, "required title") + "required ".len();
    let declaration_range = range_for(source, declaration, declaration + "title".len());
    assert!(
        with.iter()
            .any(|location| location.range == declaration_range)
    );
    assert!(
        without
            .iter()
            .all(|location| location.range != declaration_range)
    );
}

#[test]
fn references_from_store_index_use_honor_exact_use_spans_and_include_declaration() {
    let source = "\
module a

resource Book
    required title: string
    required shelf: string

store ^books(id: int): Book
    index byShelf(shelf, id)

pub fn f(): int
    return count(^books.byShelf(\"fiction\"))

pub fn g(): int
    return count(^books.byShelf(\"history\"))
";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);
    let line_index = indices.0.get(&file).unwrap();

    let use_offset = offset_of(source, ".byShelf") + 1;
    let with = references(&snapshot, &index, &indices, &file, use_offset, true)
        .expect("store index references resolve through catalog facts");
    let without = references(&snapshot, &index, &indices, &file, use_offset, false)
        .expect("store index references resolve through catalog facts");
    let with_texts: Vec<&str> = with
        .iter()
        .map(|location| range_text(source, line_index, location.range))
        .collect();
    let without_texts: Vec<&str> = without
        .iter()
        .map(|location| range_text(source, line_index, location.range))
        .collect();

    assert_eq!(with_texts, vec!["byShelf", "byShelf", "byShelf"]);
    assert_eq!(without_texts, vec!["byShelf", "byShelf"]);

    let declaration = offset_of(source, "index byShelf") + "index ".len();
    let declaration_range = range_for(source, declaration, declaration + "byShelf".len());
    assert!(
        with.iter()
            .any(|location| location.range == declaration_range)
    );
    assert!(
        without
            .iter()
            .all(|location| location.range != declaration_range)
    );
}

#[test]
fn references_of_a_param_returns_all_its_uses_shadowing_correct() {
    // The parameter `n` is used twice; an inner block shadows it with a local
    // `n` whose use must NOT be attributed to the parameter.
    let source = "\
module a

pub fn f(n: int): int
    if true
        const n: int = 99
        return n
    return n
";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);

    // Query from the outer use of the parameter (the realistic editor case: the
    // cursor sits on an identifier use). A parameter has no AST span of its own,
    // so resolving *from its declaration* would land on the enclosing function;
    // resolving from a use lands on the parameter binding.
    let outer_return = source.rfind("return n").unwrap() + "return ".len();
    let refs = references(&snapshot, &index, &indices, &file, outer_return, true)
        .expect("references for the parameter");
    let lines: Vec<u32> = refs.iter().map(|r| r.range.start.line).collect();

    // The outer use is among the references.
    let (outer_line, _) = line_col(source, outer_return);
    assert!(
        lines.contains(&outer_line),
        "the outer use of the parameter should be a reference, got {lines:?}"
    );

    // The inner `return n` is shadowed by the inner `const n`, so it must NOT be
    // attributed to the parameter.
    let inner_return = source.find("return n").unwrap() + "return ".len();
    let (inner_line, _) = line_col(source, inner_return);
    assert!(
        !lines.contains(&inner_line),
        "the shadowed inner use must NOT be a reference of the parameter, got {lines:?}"
    );
}

#[test]
fn references_can_exclude_the_declaration() {
    let source = "module a\n\npub fn f(n: int): int\n    return n\n";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);

    // Query from the use so the parameter binding resolves (its declaration has
    // no span of its own). `with` keeps the declaration; `without` drops it.
    let use_offset = offset_of(source, "return n") + "return ".len();
    let with = references(&snapshot, &index, &indices, &file, use_offset, true).unwrap();
    let without = references(&snapshot, &index, &indices, &file, use_offset, false).unwrap();
    assert_eq!(
        with.len(),
        without.len() + 1,
        "excluding the declaration drops exactly one location"
    );
}

#[test]
fn rename_of_a_local_rewrites_all_uses() {
    let source = "module a\n\npub fn f(): int\n    const n: int = 1\n    return n + n\n";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);

    let def_offset = offset_of(source, "const n") + "const ".len();
    let edit =
        rename(&index, &indices, &file, def_offset, "count").expect("a local rename succeeds");

    let url = Url::from_file_path(&file).unwrap();
    let changes = edit.changes.expect("changes present");
    let edits = &changes[&url];
    // The declaration `n` plus its two uses in `n + n`: three edits.
    assert_eq!(edits.len(), 3, "every occurrence of the local is rewritten");
    for text_edit in edits {
        assert_eq!(text_edit.new_text, "count");
    }
}

#[test]
fn rename_of_a_saved_field_is_refused() {
    let source = "\
module a

resource Book
    required title: string

store ^books(id: int): Book

pub fn f(): string
    return ^books(1).title ?? \"\"
";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);

    // Cursor on the `title` field declaration.
    let field_offset = offset_of(source, "required title") + "required ".len();
    let result = rename(&index, &indices, &file, field_offset, "name");
    assert!(
        matches!(result, Err(RenameError::SavedDataBacked)),
        "renaming a saved field must be refused, got {result:?}"
    );
}

#[test]
fn rename_of_an_imported_module_alias_is_refused_without_local_retargeting() {
    let library = "\
module shelf::books

resource Book
    required title: string

pub fn title(): string
    return \"Dune\"
";
    let app = "\
module shelf::app
use shelf::books

fn run(items: sequence[books::Book]): string
    return books::title()
";
    let (snapshot, paths, indices) =
        analyze_files(&[("shelf/books.mw", library), ("shelf/app.mw", app)]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let type_alias = offset_of(app, "sequence[books::Book]") + "sequence[".len();
    assert!(
        prepare_rename(&index, &indices, app_file, type_alias + 1).is_none(),
        "prepare_rename must not advertise editor rename for imports without a Marrow rename action"
    );
    let result = rename(&index, &indices, app_file, type_alias + 1, "volumes");
    assert_eq!(result, Err(RenameError::NotRenameable));

    let call_alias = offset_of(app, "books::title()");
    let result = rename(&index, &indices, app_file, call_alias + 1, "volumes");
    assert_eq!(result, Err(RenameError::NotRenameable));
}

#[test]
fn rename_off_any_symbol_is_no_symbol() {
    let source = "module a\n\npub fn f(): int\n    return 1\n";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);
    // Offset 0 is the `module` keyword — no symbol.
    let result = rename(&index, &indices, &file, 0, "x");
    assert_eq!(result, Err(RenameError::NoSymbol));
}

#[test]
fn rename_rejects_invalid_replacement_text() {
    let source = "module a\n\npub fn f(): int\n    const n: int = 1\n    return n\n";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);
    let def_offset = offset_of(source, "const n") + "const ".len();

    assert!(
        rename(&index, &indices, &file, def_offset, "count2").is_ok(),
        "a normal identifier remains a valid rename target"
    );
    assert!(
        rename(&index, &indices, &file, def_offset, "at").is_ok(),
        "non-keyword identifiers remain valid rename targets"
    );
    assert_eq!(
        rename(&index, &indices, &file, def_offset, "two words"),
        Err(RenameError::InvalidName)
    );
    assert_eq!(
        rename(&index, &indices, &file, def_offset, "return"),
        Err(RenameError::InvalidName)
    );
    // Marrow keywords are not valid identifiers; renaming a symbol to one
    // would produce source that no longer parses.
    for keyword in ["index", "unique", "ErrorCode", "store", "evolve", "is"] {
        assert_eq!(
            rename(&index, &indices, &file, def_offset, keyword),
            Err(RenameError::InvalidName),
            "renaming to the keyword `{keyword}` must be rejected"
        );
    }
}

#[test]
fn prepare_rename_returns_the_identifier_range() {
    let source = "module a\n\npub fn f(): int\n    const n: int = 1\n    return n\n";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);

    let use_offset = offset_of(source, "return n") + "return ".len();
    let range = prepare_rename(&index, &indices, &file, use_offset)
        .expect("a renameable identifier at the use");
    let (line, character) = line_col(source, use_offset);
    assert_eq!(range.start.line, line);
    assert_eq!(range.start.character, character);
    assert_eq!(range.end.character, character + 1, "the range covers `n`");
}

#[test]
fn definition_from_enum_type_annotation_jumps_to_enum_name() {
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
    let (snapshot, paths, indices) =
        analyze_files(&[("a/b.mw", status_source), ("app.mw", app_source)]);
    let status_file = &paths[0];
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let annotation = offset_of(app_source, "b::Status") + "b::".len();
    let location = definition(&snapshot, &index, &indices, app_file, annotation + 1)
        .expect("enum annotation resolves to its declaration");
    let line_index = indices.0.get(status_file).unwrap();

    assert_eq!(
        range_text(status_source, line_index, location.range),
        "Status"
    );
    let enum_name = offset_of(status_source, "enum Status") + "enum ".len();
    let (line, character) = line_col(status_source, enum_name);
    assert_eq!(location.range.start.line, line);
    assert_eq!(location.range.start.character, character);
}

#[test]
fn catalog_definition_from_bare_foreign_enum_type_annotation_jumps_to_enum_name() {
    let status_source = "\
module a::b
pub enum Status
    active
    archived
";
    let app_source = "\
module app

fn f(s: Status)
    return
";
    let (snapshot, paths, indices) =
        analyze_files(&[("a/b.mw", status_source), ("app.mw", app_source)]);
    let status_file = &paths[0];
    let app_file = &paths[1];

    let annotation = offset_of(app_source, "s: Status") + "s: ".len();
    let location = super::catalog_uses::definition(&snapshot, &indices, app_file, annotation + 1)
        .expect("enum annotation resolves through catalog facts");
    let line_index = indices.0.get(status_file).unwrap();

    assert_eq!(
        range_text(status_source, line_index, location.range),
        "Status"
    );
}

#[test]
fn definition_from_evolve_transform_type_annotation_jumps_to_resource() {
    let source = "\
module a

resource Book
    required title: string
    required slug: string

store ^books(id: int): Book

evolve
    transform Book.title
        const draft: Book = Book(title: old.slug, slug: old.slug)
        return draft.title
";
    let (snapshot, file, indices) = analyze(source);
    let index = build_binding_index(&snapshot);

    let annotation = offset_of(source, "draft: Book") + "draft: ".len();
    let location = definition(&snapshot, &index, &indices, &file, annotation + 1)
        .expect("transform body type annotation resolves through BindingIndex");
    let line_index = indices.0.get(&file).unwrap();

    assert_eq!(range_text(source, line_index, location.range), "Book");
    let declaration = offset_of(source, "resource Book") + "resource ".len();
    assert_eq!(
        location.range,
        range_for(source, declaration, declaration + "Book".len())
    );
}

#[test]
fn module_path_alias_prefix_definition_from_imported_call_jumps_to_use_leaf() {
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
    let (snapshot, paths, indices) = analyze_files(&[
        ("shelf/books.mw", books_source),
        ("shelf/app.mw", app_source),
    ]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);
    let app_index = indices.0.get(app_file).unwrap();

    let prefix = offset_of(app_source, "return books::titleOf") + "return ".len();
    let location = definition(&snapshot, &index, &indices, app_file, prefix + 1)
        .expect("imported alias prefix resolves through the binding index");

    assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
    assert_eq!(range_text(app_source, app_index, location.range), "books");
    let import_leaf = offset_of(app_source, "use shelf::books") + "use shelf::".len();
    let (line, character) = line_col(app_source, import_leaf);
    assert_eq!(location.range.start.line, line);
    assert_eq!(location.range.start.character, character);
}

#[test]
fn module_path_leaf_definition_from_imported_call_stays_on_binding_index() {
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
    let (snapshot, paths, indices) = analyze_files(&[
        ("shelf/books.mw", books_source),
        ("shelf/app.mw", app_source),
    ]);
    let books_file = &paths[0];
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let leaf = offset_of(app_source, "books::titleOf") + "books::".len();
    let location = definition(&snapshot, &index, &indices, app_file, leaf + 1)
        .expect("leaf call still resolves through the binding index");
    let line_index = indices.0.get(books_file).unwrap();

    assert_eq!(location.uri, Url::from_file_path(books_file).unwrap());
    assert_eq!(
        range_text(books_source, line_index, location.range),
        "titleOf"
    );
    let function_name = offset_of(books_source, "fn titleOf") + "fn ".len();
    let (line, character) = line_col(books_source, function_name);
    assert_eq!(location.range.start.line, line);
    assert_eq!(location.range.start.character, character);
}

#[test]
fn definition_from_qualified_resource_constructor_leaf_jumps_to_foreign_resource() {
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
    let (snapshot, paths, indices) = analyze_files(&[
        ("shelf/state.mw", state_source),
        ("shelf/app.mw", app_source),
    ]);
    let state_file = &paths[0];
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let book = offset_of(app_source, "state::Book(title") + "state::".len();
    let location = definition(&snapshot, &index, &indices, app_file, book + 1)
        .expect("qualified resource constructor leaf resolves to the foreign resource");
    let state_index = indices.0.get(state_file).unwrap();

    assert_eq!(location.uri, Url::from_file_path(state_file).unwrap());
    assert_eq!(
        range_text(state_source, state_index, location.range),
        "Book"
    );
}

#[test]
fn definition_from_same_named_local_and_foreign_resource_uses_the_qualified_target() {
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

pub fn make_foreign()
    const book = state::Book(title: \"x\")

pub fn make_local()
    const book = Book(label: \"local\")
";
    let (snapshot, paths, indices) = analyze_files(&[
        ("shelf/state.mw", state_source),
        ("shelf/app.mw", app_source),
    ]);
    let state_file = &paths[0];
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);
    let state_index = indices.0.get(state_file).unwrap();
    let app_index = indices.0.get(app_file).unwrap();

    let qualified_book = offset_of(app_source, "state::Book(title") + "state::".len();
    let qualified_location = definition(&snapshot, &index, &indices, app_file, qualified_book + 1)
        .expect("qualified constructor resolves to the imported resource");
    assert_eq!(
        qualified_location.uri,
        Url::from_file_path(state_file).unwrap()
    );
    assert_eq!(
        range_text(state_source, state_index, qualified_location.range),
        "Book"
    );

    let bare_book = offset_of(app_source, "Book(label: \"local\")");
    let bare_location = definition(&snapshot, &index, &indices, app_file, bare_book + 1)
        .expect("bare constructor resolves to the local resource");
    assert_eq!(bare_location.uri, Url::from_file_path(app_file).unwrap());
    assert_eq!(
        range_text(app_source, app_index, bare_location.range),
        "Book"
    );
}

#[test]
fn module_path_alias_definition_from_use_leaf_can_resolve_to_itself() {
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
    let (snapshot, paths, indices) = analyze_files(&[
        ("shelf/books.mw", books_source),
        ("shelf/app.mw", app_source),
    ]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);
    let app_index = indices.0.get(app_file).unwrap();

    let imported_leaf = offset_of(app_source, "use shelf::books") + "use shelf::".len();
    let location = definition(&snapshot, &index, &indices, app_file, imported_leaf + 1)
        .expect("use leaf aliases can resolve to their own binding fact");

    assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
    assert_eq!(range_text(app_source, app_index, location.range), "books");
    let (line, character) = line_col(app_source, imported_leaf);
    assert_eq!(location.range.start.line, line);
    assert_eq!(location.range.start.character, character);
}

#[test]
fn module_path_references_from_imported_module_alias_include_definition_and_prefix_uses() {
    let books_source = "\
module shelf::books

resource Book
    required title: string

pub fn titleOf(): string
    return \"Dune\"
";
    let app_source = "\
module shelf::app

use shelf::books

fn run(items: sequence[books::Book]): string
    return books::titleOf()
";
    let (snapshot, paths, indices) = analyze_files(&[
        ("shelf/books.mw", books_source),
        ("shelf/app.mw", app_source),
    ]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);
    let app_index = indices.0.get(app_file).unwrap();

    let call_alias = offset_of(app_source, "return books::titleOf") + "return ".len();
    let with = references(&snapshot, &index, &indices, app_file, call_alias + 1, true)
        .expect("module alias references resolve through the binding index");
    let without = references(&snapshot, &index, &indices, app_file, call_alias + 1, false)
        .expect("module alias references resolve through the binding index");
    let with_texts: Vec<&str> = with
        .iter()
        .map(|location| range_text(app_source, app_index, location.range))
        .collect();
    let without_texts: Vec<&str> = without
        .iter()
        .map(|location| range_text(app_source, app_index, location.range))
        .collect();

    assert_eq!(with_texts, vec!["books", "books", "books"]);
    assert_eq!(without_texts, vec!["books", "books"]);

    let import_leaf = offset_of(app_source, "use shelf::books") + "use shelf::".len();
    let declaration_range = range_for(app_source, import_leaf, import_leaf + "books".len());
    assert!(
        with.iter()
            .any(|location| location.range == declaration_range)
    );
    assert!(
        without
            .iter()
            .all(|location| location.range != declaration_range)
    );
}

#[test]
fn module_path_prefix_definition_for_std_path_returns_none() {
    let clock_source = "\
module std::clock

const TICK: int = 1
";
    let app_source = "\
module shelf::app

use shelf::clock

pub fn run(): instant
    return std::clock::now()
";
    let (snapshot, paths, indices) =
        analyze_files(&[("std/clock.mw", clock_source), ("shelf/app.mw", app_source)]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let std_clock = offset_of(app_source, "std::clock::now") + "std::".len();
    let location = definition(&snapshot, &index, &indices, app_file, std_clock + 1);

    assert_eq!(
        location, None,
        "std module prefixes must not fabricate a project location"
    );
}

#[test]
fn module_path_alias_prefix_definition_for_aliased_std_path_jumps_to_use_leaf() {
    let clock_source = "\
module std::clock

const TICK: int = 1
";
    let app_source = "\
module shelf::app

use std::clock

pub fn run(): instant
    return clock::now()
";
    let (snapshot, paths, indices) =
        analyze_files(&[("std/clock.mw", clock_source), ("shelf/app.mw", app_source)]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);
    let app_index = indices.0.get(app_file).unwrap();

    let std_clock = offset_of(app_source, "clock::now");
    let location = definition(&snapshot, &index, &indices, app_file, std_clock + 1)
        .expect("imported std alias prefix resolves through the binding index");

    assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
    assert_eq!(range_text(app_source, app_index, location.range), "clock");
    let import_leaf = offset_of(app_source, "use std::clock") + "use std::".len();
    let (line, character) = line_col(app_source, import_leaf);
    assert_eq!(location.range.start.line, line);
    assert_eq!(location.range.start.character, character);
}

#[test]
fn module_path_prefix_definition_blocks_project_std_namespace_module_without_canonical_fact() {
    let custom_source = "\
module std::custom

pub fn tick(): int
    return 1
";
    let app_source = "\
module app

pub fn run(): int
    return std::custom::tick()
";
    let (snapshot, paths, indices) =
        analyze_files(&[("std/custom.mw", custom_source), ("app.mw", app_source)]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let custom = offset_of(app_source, "std::custom::tick") + "std::".len();
    assert_eq!(
        definition(&snapshot, &index, &indices, app_file, custom + 1),
        None
    );
}

#[test]
fn module_path_prefix_definition_does_not_steal_enum_literal_head() {
    let status_module_source = "\
module status

pub fn helper(): int
    return 1
";
    let app_source = "\
module app

enum status
    active

pub fn run(): status
    return status::active
";
    let (snapshot, paths, indices) =
        analyze_files(&[("status.mw", status_module_source), ("app.mw", app_source)]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let enum_head = offset_of(app_source, "return status::active") + "return ".len();
    let location = definition(&snapshot, &index, &indices, app_file, enum_head + 1)
        .expect("enum literal head resolves through the binding index");
    let line_index = indices.0.get(app_file).unwrap();

    assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
    assert_eq!(range_text(app_source, line_index, location.range), "status");
    let enum_name = offset_of(app_source, "enum status") + "enum ".len();
    let (line, character) = line_col(app_source, enum_name);
    assert_eq!(location.range.start.line, line);
    assert_eq!(location.range.start.character, character);
}

#[test]
fn module_path_prefix_definition_does_not_steal_nested_enum_literal_head() {
    let cat_module_source = "\
module cat

pub fn helper(): int
    return 1
";
    let app_source = "\
module app

enum cat
    category tiger
        paw

pub fn run(): cat
    return cat::tiger::paw
";
    let (snapshot, paths, indices) =
        analyze_files(&[("cat.mw", cat_module_source), ("app.mw", app_source)]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let enum_head = offset_of(app_source, "return cat::tiger::paw") + "return ".len();
    let location = definition(&snapshot, &index, &indices, app_file, enum_head + 1)
        .expect("nested enum literal head resolves through the binding index");
    let line_index = indices.0.get(app_file).unwrap();

    assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
    assert_eq!(range_text(app_source, line_index, location.range), "cat");
    let enum_name = offset_of(app_source, "enum cat") + "enum ".len();
    let (line, character) = line_col(app_source, enum_name);
    assert_eq!(location.range.start.line, line);
    assert_eq!(location.range.start.character, character);
}

#[test]
fn module_path_prefix_definition_does_not_steal_nested_enum_member_segment() {
    let tiger_module_source = "\
module cat::tiger

pub fn helper(): int
    return 1
";
    let app_source = "\
module app

enum cat
    category tiger
        paw

pub fn run(): cat
    return cat::tiger::paw
";
    let (snapshot, paths, indices) = analyze_files(&[
        ("cat/tiger.mw", tiger_module_source),
        ("app.mw", app_source),
    ]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let member_segment = offset_of(app_source, "return cat::tiger::paw") + "return cat::".len();
    let location = definition(&snapshot, &index, &indices, app_file, member_segment + 1)
        .expect("nested enum member segment resolves through the binding index");
    let line_index = indices.0.get(app_file).unwrap();

    assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
    assert_eq!(range_text(app_source, line_index, location.range), "tiger");
    let member_name = offset_of(app_source, "category tiger") + "category ".len();
    let (line, character) = line_col(app_source, member_name);
    assert_eq!(location.range.start.line, line);
    assert_eq!(location.range.start.character, character);
}

#[test]
fn module_path_prefix_definition_does_not_steal_imported_module_named_like_local_resource() {
    let book_source = "\
module shelf::book

pub fn lookup(): int
    return 1
";
    let app_source = "\
module app

use shelf::book

resource book
    required title: string

store ^books(id: int): book

pub fn run(): int
    return book::lookup()
";
    let (snapshot, paths, indices) =
        analyze_files(&[("shelf/book.mw", book_source), ("app.mw", app_source)]);
    let book_file = &paths[0];
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);
    let book_index = indices.0.get(book_file).unwrap();
    let app_index = indices.0.get(app_file).unwrap();

    let prefix = offset_of(app_source, "return book::lookup") + "return ".len();
    let prefix_location = definition(&snapshot, &index, &indices, app_file, prefix + 1)
        .expect("imported module alias wins over same-named local resource at the prefix");
    assert_eq!(prefix_location.uri, Url::from_file_path(app_file).unwrap());
    assert_eq!(
        range_text(app_source, app_index, prefix_location.range),
        "book"
    );
    let import_leaf = offset_of(app_source, "use shelf::book") + "use shelf::".len();
    let (line, character) = line_col(app_source, import_leaf);
    assert_eq!(prefix_location.range.start.line, line);
    assert_eq!(prefix_location.range.start.character, character);

    let leaf = offset_of(app_source, "return book::lookup") + "return book::".len();
    let leaf_location = definition(&snapshot, &index, &indices, app_file, leaf + 1)
        .expect("aliased leaf resolves to the imported function");
    assert_eq!(leaf_location.uri, Url::from_file_path(book_file).unwrap());
    assert_eq!(
        range_text(book_source, book_index, leaf_location.range),
        "lookup"
    );
}

#[test]
fn module_path_alias_prefix_and_function_leaf_use_binding_index_facts() {
    let book_source = "\
module shelf::book

pub fn lookup(): int
    return 1
";
    let app_source = "\
module app

use shelf::book

pub fn run(): int
    return book::lookup()
";
    let (snapshot, paths, indices) =
        analyze_files(&[("shelf/book.mw", book_source), ("app.mw", app_source)]);
    let book_file = &paths[0];
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);
    let book_index = indices.0.get(book_file).unwrap();
    let app_index = indices.0.get(app_file).unwrap();

    let prefix = offset_of(app_source, "return book::lookup") + "return ".len();
    let prefix_location = definition(&snapshot, &index, &indices, app_file, prefix + 1)
        .expect("imported module alias prefix resolves through the binding index");
    assert_eq!(prefix_location.uri, Url::from_file_path(app_file).unwrap());
    assert_eq!(
        range_text(app_source, app_index, prefix_location.range),
        "book"
    );
    let import_leaf = offset_of(app_source, "use shelf::book") + "use shelf::".len();
    let (line, character) = line_col(app_source, import_leaf);
    assert_eq!(prefix_location.range.start.line, line);
    assert_eq!(prefix_location.range.start.character, character);

    let leaf = offset_of(app_source, "book::lookup") + "book::".len();
    let leaf_location = definition(&snapshot, &index, &indices, app_file, leaf + 1)
        .expect("leaf call resolves to the imported function");
    assert_eq!(leaf_location.uri, Url::from_file_path(book_file).unwrap());
    assert_eq!(
        range_text(book_source, book_index, leaf_location.range),
        "lookup"
    );
}

#[test]
fn module_path_prefix_definition_named_argument_colon_is_not_type_annotation() {
    let book_source = "\
module shelf::book

pub fn lookup(): int
    return 1
";
    let app_source = "\
module app

use shelf::book

resource book
    required title: string

store ^books(id: int): book

pub fn wrap(value: int): int
    return value

pub fn run(): int
    return wrap(value: book::lookup())
";
    let (snapshot, paths, indices) =
        analyze_files(&[("shelf/book.mw", book_source), ("app.mw", app_source)]);
    let book_file = &paths[0];
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);
    let book_index = indices.0.get(book_file).unwrap();
    let app_index = indices.0.get(app_file).unwrap();

    let prefix =
        offset_of(app_source, "return wrap(value: book::lookup") + "return wrap(value: ".len();
    let prefix_location = definition(&snapshot, &index, &indices, app_file, prefix + 1)
        .expect("named argument call prefix resolves through the binding index");
    assert_eq!(prefix_location.uri, Url::from_file_path(app_file).unwrap());
    assert_eq!(
        range_text(app_source, app_index, prefix_location.range),
        "book"
    );
    let import_leaf = offset_of(app_source, "use shelf::book") + "use shelf::".len();
    let (line, character) = line_col(app_source, import_leaf);
    assert_eq!(prefix_location.range.start.line, line);
    assert_eq!(prefix_location.range.start.character, character);

    let leaf = offset_of(app_source, "return wrap(value: book::lookup")
        + "return wrap(value: book::".len();
    let leaf_location = definition(&snapshot, &index, &indices, app_file, leaf + 1)
        .expect("named argument aliased call leaf resolves to the imported function");
    assert_eq!(leaf_location.uri, Url::from_file_path(book_file).unwrap());
    assert_eq!(
        range_text(book_source, book_index, leaf_location.range),
        "lookup"
    );
}

#[test]
fn module_path_prefix_definition_repeated_leaf_is_blocked_without_canonical_fact() {
    let foo_source = "\
module foo::foo

pub fn title(): string
    return \"nested\"
";
    let app_source = "\
module app

pub fn run(): string
    return foo::foo::title()
";
    let (snapshot, paths, indices) =
        analyze_files(&[("foo/foo.mw", foo_source), ("app.mw", app_source)]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let second_prefix = offset_of(app_source, "foo::foo::title") + "foo::".len();
    assert_eq!(
        definition(&snapshot, &index, &indices, app_file, second_prefix + 1),
        None
    );
}

#[test]
fn module_path_namespace_only_prefix_in_fully_qualified_call_returns_none() {
    let books_source = "\
module shelf::books

pub fn titleOf(): string
    return \"Dune\"
";
    let app_source = "\
module shelf::app

pub fn run(): string
    return shelf::books::titleOf()
";
    let (snapshot, paths, indices) = analyze_files(&[
        ("shelf/books.mw", books_source),
        ("shelf/app.mw", app_source),
    ]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let namespace = offset_of(app_source, "return shelf::books::titleOf") + "return ".len();
    let location = definition(&snapshot, &index, &indices, app_file, namespace + 1);

    assert_eq!(
        location, None,
        "namespace-only prefixes should not fall through to the call leaf"
    );
}

#[test]
fn module_path_prefix_definition_blocks_keyword_like_segments_without_binding_fact() {
    let bytes_source = "\
module shelf::bytes

pub fn size(): int
    return 1
";
    let app_source = "\
module shelf::app

use shelf::bytes

pub fn run(): int
    return bytes::size()
";
    let (snapshot, paths, indices) = analyze_files(&[
        ("shelf/bytes.mw", bytes_source),
        ("shelf/app.mw", app_source),
    ]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let prefix = offset_of(app_source, "return bytes::size") + "return ".len();
    assert_eq!(
        definition(&snapshot, &index, &indices, app_file, prefix + 1),
        None
    );
}

#[test]
fn references_from_enum_type_annotation_stay_on_that_enum() {
    let status_source = "\
module a::b
pub enum Status
    active
    archived
";
    let app_source = "\
module app
use a::b

enum Color
    active

fn f(): b::Status
    const current: b::Status = b::Status::active
    const color: Color = Color::active
    return current
";
    let (snapshot, paths, indices) =
        analyze_files(&[("a/b.mw", status_source), ("app.mw", app_source)]);
    let status_file = &paths[0];
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let annotation = offset_of(app_source, "const current: b::Status") + "const current: b::".len();
    let refs = references(&snapshot, &index, &indices, app_file, annotation + 1, true)
        .expect("references for enum annotation");
    let texts: Vec<&str> = refs
        .iter()
        .map(|location| {
            let path = location.uri.to_file_path().unwrap();
            let (source, line_index) = if path == *status_file {
                (status_source, indices.0.get(status_file).unwrap())
            } else {
                (app_source, indices.0.get(app_file).unwrap())
            };
            range_text(source, line_index, location.range)
        })
        .collect();

    assert_eq!(
        texts,
        vec!["Status", "Status", "Status"],
        "declaration, return annotation, and local annotation are enum type refs"
    );

    let without_decl =
        references(&snapshot, &index, &indices, app_file, annotation + 1, false).unwrap();
    assert_eq!(
        without_decl.len(),
        refs.len() - 1,
        "excluding the declaration drops only the enum declaration"
    );
    assert!(
        texts
            .iter()
            .all(|text| *text != "active" && *text != "Color"),
        "enum references should not include member values or another enum: {texts:?}"
    );
}

#[test]
fn catalog_references_from_enum_type_annotation_include_enum_sites_only() {
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
    const current: b::Status = b::Status::active
    return current
";
    let (snapshot, paths, indices) =
        analyze_files(&[("a/b.mw", status_source), ("app.mw", app_source)]);
    let status_file = &paths[0];
    let app_file = &paths[1];

    let annotation = offset_of(app_source, "const current: b::Status") + "const current: b::".len();
    let refs = super::catalog_uses::references(&snapshot, &indices, app_file, annotation + 1, true)
        .expect("enum annotation references resolve through catalog facts");
    let texts: Vec<&str> = refs
        .iter()
        .map(|location| {
            let path = location.uri.to_file_path().unwrap();
            let (source, line_index) = if path == *status_file {
                (status_source, indices.0.get(status_file).unwrap())
            } else {
                (app_source, indices.0.get(app_file).unwrap())
            };
            range_text(source, line_index, location.range)
        })
        .collect();

    assert_eq!(
        texts,
        vec!["Status", "Status", "Status"],
        "catalog enum annotation references should not include enum literal heads"
    );
}

#[test]
fn references_from_enum_literal_prefix_stay_on_binding_index() {
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
    const current: b::Status = b::Status::active
    return b::Status::archived
";
    let (snapshot, paths, indices) =
        analyze_files(&[("a/b.mw", status_source), ("app.mw", app_source)]);
    let status_file = &paths[0];
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);

    let literal_prefix = offset_of(app_source, "b::Status::active") + "b::".len();
    let refs = references(
        &snapshot,
        &index,
        &indices,
        app_file,
        literal_prefix + 1,
        true,
    )
    .expect("references for enum literal prefix");
    let texts: Vec<&str> = refs
        .iter()
        .map(|location| {
            let path = location.uri.to_file_path().unwrap();
            let (source, line_index) = if path == *status_file {
                (status_source, indices.0.get(status_file).unwrap())
            } else {
                (app_source, indices.0.get(app_file).unwrap())
            };
            range_text(source, line_index, location.range)
        })
        .collect();

    assert_eq!(
        texts,
        vec!["Status", "Status", "Status", "Status", "Status"],
        "catalog use-site references must not truncate enum references from type annotations"
    );
}

#[test]
fn rename_from_enum_type_annotation_rejects_saved_data_backed_renames() {
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
    const current: b::Status = b::Status::active
    return current
";
    let (snapshot, paths, indices) =
        analyze_files(&[("a/b.mw", status_source), ("app.mw", app_source)]);
    let app_file = &paths[1];
    let index = build_binding_index(&snapshot);
    let app_index = indices.0.get(app_file).unwrap();

    let annotation = offset_of(app_source, "const current: b::Status") + "const current: b::".len();
    let prepared = prepare_rename(&index, &indices, app_file, annotation + 1)
        .expect("enum annotation can prepare rename");
    assert_eq!(range_text(app_source, app_index, prepared), "Status");

    let result = rename(&index, &indices, app_file, annotation + 1, "State");
    assert!(
        matches!(result, Err(RenameError::SavedDataBacked)),
        "enum names are catalog-backed and must not be source-renamed: {result:?}"
    );
}
