//! Document and workspace symbols from a parsed file and a checked program.
//!
//! Document symbols are the outline of one file: a [`DocumentSymbol`] per
//! declaration (constant, enum, resource, function), with enum members and resource
//! members nested beneath their parents. Workspace symbols flatten every checked
//! module's functions, resources, enums, and constants into a project-wide list an
//! editor's "go to symbol in workspace" searches. Both read only the parse and the
//! checked program; nothing here re-parses or re-checks.

use lsp_types::{DocumentSymbol, Location, SymbolInformation, SymbolKind, Url};
use marrow_check::CheckedProgram;
use marrow_syntax::{
    Declaration, EnumDecl, EnumMember, FieldDecl, FunctionDecl, GroupDecl, IndexDecl, ResourceDecl,
    ResourceMember, SourceFile, SourceSpan, StoreDecl,
};

use crate::positions::LineIndex;

/// The outline of one file: a symbol per top-level declaration, in source order.
///
/// `index` supplies the file's text and position mapping. A symbol's `range`
/// covers its whole declaration; its `selection_range` covers just the name, the
/// span an editor highlights and reveals when the symbol is chosen.
pub fn document_symbols(file: &SourceFile, index: &LineIndex) -> Vec<DocumentSymbol> {
    file.declarations
        .iter()
        .map(|declaration| declaration_symbol(declaration, index))
        .collect()
}

fn declaration_symbol(declaration: &Declaration, index: &LineIndex) -> DocumentSymbol {
    match declaration {
        Declaration::Const(constant) => symbol(
            &constant.name,
            constant.ty.as_ref().map(|ty| ty.text.clone()),
            SymbolKind::CONSTANT,
            constant.span,
            index,
            None,
        ),
        Declaration::Function(function) => symbol(
            &function.name,
            Some(function_signature(function)),
            SymbolKind::FUNCTION,
            function.span,
            index,
            None,
        ),
        Declaration::Enum(enum_decl) => enum_symbol(enum_decl, index),
        Declaration::Resource(resource) => resource_symbol(resource, index),
        Declaration::Store(store) => store_symbol(store, index),
        Declaration::Evolve(evolve) => symbol(
            "evolve",
            None,
            SymbolKind::NAMESPACE,
            evolve.span,
            index,
            None,
        ),
    }
}

fn enum_symbol(enum_decl: &EnumDecl, index: &LineIndex) -> DocumentSymbol {
    let children = enum_decl
        .members
        .iter()
        .map(|member| enum_member_symbol(member, index))
        .collect();
    symbol(
        &enum_decl.name,
        None,
        SymbolKind::ENUM,
        enum_decl.span,
        index,
        Some(children),
    )
}

fn enum_member_symbol(member: &EnumMember, index: &LineIndex) -> DocumentSymbol {
    symbol(
        &member.name,
        None,
        SymbolKind::ENUM_MEMBER,
        member.span,
        index,
        None,
    )
}

fn resource_symbol(resource: &ResourceDecl, index: &LineIndex) -> DocumentSymbol {
    let children = resource
        .members
        .iter()
        .map(|member| member_symbol(member, index))
        .collect();
    symbol(
        &resource.name,
        None,
        SymbolKind::STRUCT,
        resource.span,
        index,
        Some(children),
    )
}

fn member_symbol(member: &ResourceMember, index: &LineIndex) -> DocumentSymbol {
    match member {
        ResourceMember::Field(field) => field_symbol(field, index),
        ResourceMember::Group(group) => group_symbol(group, index),
    }
}

fn store_symbol(store: &StoreDecl, index: &LineIndex) -> DocumentSymbol {
    let children = store
        .indexes
        .iter()
        .map(|idx| index_symbol(idx, index))
        .collect();
    symbol(
        &format!("^{}", store.root.root),
        Some(store.resource.clone()),
        SymbolKind::OBJECT,
        store.span,
        index,
        Some(children),
    )
}

fn field_symbol(field: &FieldDecl, index: &LineIndex) -> DocumentSymbol {
    symbol(
        &field.name,
        Some(field.ty.text.clone()),
        SymbolKind::FIELD,
        field.span,
        index,
        None,
    )
}

fn group_symbol(group: &GroupDecl, index: &LineIndex) -> DocumentSymbol {
    let children = group
        .members
        .iter()
        .map(|member| member_symbol(member, index))
        .collect();
    symbol(
        &group.name,
        None,
        SymbolKind::OBJECT,
        group.span,
        index,
        Some(children),
    )
}

fn index_symbol(idx: &IndexDecl, index: &LineIndex) -> DocumentSymbol {
    symbol(
        &idx.name,
        Some(format!("index({})", idx.args.join(", "))),
        SymbolKind::KEY,
        idx.span,
        index,
        None,
    )
}

/// Build one symbol, computing its full range from `span` and its selection range
/// from where `name` sits inside that span.
fn symbol(
    name: &str,
    detail: Option<String>,
    kind: SymbolKind,
    span: SourceSpan,
    index: &LineIndex,
    children: Option<Vec<DocumentSymbol>>,
) -> DocumentSymbol {
    let range = index.range(span.start_byte, span.end_byte);
    let (name_start, name_end) = name_span(name, span, index.text());
    #[allow(deprecated)]
    DocumentSymbol {
        name: name.to_string(),
        detail,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range: index.range(name_start, name_end),
        children,
    }
}

/// The byte span of `name` within a declaration's `span`, for the selection
/// range. The name is the first occurrence of the identifier inside the
/// declaration's source slice (declarations open with a keyword, so the name
/// never precedes it); if it cannot be found the whole declaration span stands in.
fn name_span(name: &str, span: SourceSpan, text: &str) -> (usize, usize) {
    let slice = &text[span.start_byte..span.end_byte];
    match slice.find(name) {
        Some(offset) => {
            let start = span.start_byte + offset;
            (start, start + name.len())
        }
        None => (span.start_byte, span.end_byte),
    }
}

/// A function's one-line signature for the symbol detail: its parameters and
/// return type as written, e.g. `(n: int): int`.
fn function_signature(function: &FunctionDecl) -> String {
    let params = function
        .params
        .iter()
        .map(|param| format!("{}: {}", param.name, param.ty.text))
        .collect::<Vec<_>>()
        .join(", ");
    match &function.return_type {
        Some(ty) => format!("({params}): {}", ty.text),
        None => format!("({params})"),
    }
}

/// Every checked module's functions, resources, enums, and constants as a flat,
/// project-wide list for "go to symbol in workspace". Each symbol is located by
/// the `file://` URL of its module's source file and the declaration's range,
/// computed from that file's on-disk text; a file whose URL or text is
/// unavailable contributes no symbols.
pub fn workspace_symbols(program: &CheckedProgram) -> Vec<SymbolInformation> {
    let mut symbols = Vec::new();
    for module in &program.modules {
        let Some(url) = Url::from_file_path(&module.source_file).ok() else {
            continue;
        };
        let index = LineIndex::new(read_to_string(&module.source_file));
        for function in &module.functions {
            symbols.push(flat_symbol(
                &function.name,
                SymbolKind::FUNCTION,
                &url,
                function.span,
                &index,
                &module.name,
            ));
        }
        for constant in &module.constants {
            symbols.push(flat_symbol(
                &constant.name,
                SymbolKind::CONSTANT,
                &url,
                constant.span,
                &index,
                &module.name,
            ));
        }
        for resource in &module.resources {
            // A resource schema carries its name but not its declaration span; the
            // module span locates it well enough for a flat jump target.
            symbols.push(flat_symbol(
                &resource.name,
                SymbolKind::STRUCT,
                &url,
                module.span,
                &index,
                &module.name,
            ));
        }
        for enum_schema in &module.enums {
            // Enum schemas carry their name but not their declaration span; the
            // module span locates them well enough for a flat jump target.
            symbols.push(flat_symbol(
                &enum_schema.name,
                SymbolKind::ENUM,
                &url,
                module.span,
                &index,
                &module.name,
            ));
        }
    }
    symbols
}

fn flat_symbol(
    name: &str,
    kind: SymbolKind,
    url: &Url,
    span: SourceSpan,
    index: &LineIndex,
    module: &str,
) -> SymbolInformation {
    #[allow(deprecated)]
    SymbolInformation {
        name: name.to_string(),
        kind,
        tags: None,
        deprecated: None,
        location: Location {
            uri: url.clone(),
            range: index.range(span.start_byte, span.end_byte),
        },
        container_name: Some(module.to_string()),
    }
}

fn read_to_string(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_check::{ProjectSources, analyze_project};
    use marrow_project::parse_config;
    use marrow_syntax::parse_source;

    fn parse(source: &str) -> SourceFile {
        parse_source(source).file
    }

    #[test]
    fn document_symbols_list_a_resource_and_store_with_nested_members() {
        let source = "\
module a

resource Book
    required title: string
    tags(pos: int): string
    notes(noteId: string)
        text: string

store ^books(id: int): Book
    index byShelf(shelf, id)
";
        let index = LineIndex::new(source);
        let symbols = document_symbols(&parse(source), &index);

        assert_eq!(symbols.len(), 2);
        let book = &symbols[0];
        assert_eq!(book.name, "Book");
        assert_eq!(book.kind, SymbolKind::STRUCT);

        let children = book.children.as_ref().expect("resource has members");
        let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["title", "tags", "notes"]);

        assert_eq!(children[0].kind, SymbolKind::FIELD); // title
        assert_eq!(children[1].kind, SymbolKind::FIELD); // tags (keyed leaf)
        assert_eq!(children[2].kind, SymbolKind::OBJECT); // notes (group)

        // The group recurses to its own field.
        let notes = &children[2];
        let group_children = notes.children.as_ref().expect("group has members");
        assert_eq!(group_children.len(), 1);
        assert_eq!(group_children[0].name, "text");
        assert_eq!(group_children[0].kind, SymbolKind::FIELD);

        let store = &symbols[1];
        assert_eq!(store.name, "^books");
        assert_eq!(store.detail.as_deref(), Some("Book"));
        assert_eq!(store.kind, SymbolKind::OBJECT);
        let indexes = store.children.as_ref().expect("store has indexes");
        assert_eq!(indexes.len(), 1);
        assert_eq!(indexes[0].name, "byShelf");
        assert_eq!(indexes[0].kind, SymbolKind::KEY);
    }

    #[test]
    fn document_symbol_selection_range_covers_the_name() {
        let source = "module a\n\npub fn answer(): int\n    return 1\n";
        let index = LineIndex::new(source);
        let symbols = document_symbols(&parse(source), &index);
        assert_eq!(symbols.len(), 1);
        let function = &symbols[0];
        assert_eq!(function.name, "answer");
        assert_eq!(function.kind, SymbolKind::FUNCTION);
        assert_eq!(function.detail.as_deref(), Some("(): int"));

        // The selection range is the `answer` identifier on line 2.
        let start = function.selection_range.start;
        assert_eq!(start.line, 2);
        assert_eq!(start.character, "pub fn ".len() as u32);
        assert_eq!(
            function.selection_range.end.character,
            ("pub fn ".len() + "answer".len()) as u32
        );
    }

    #[test]
    fn document_symbols_cover_constants_and_functions() {
        let source = "module a\n\nconst LIMIT: int = 10\n\npub fn f(n: int): int\n    return n\n";
        let index = LineIndex::new(source);
        let symbols = document_symbols(&parse(source), &index);
        let kinds: Vec<SymbolKind> = symbols.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [SymbolKind::CONSTANT, SymbolKind::FUNCTION]);
        assert_eq!(symbols[0].name, "LIMIT");
        assert_eq!(symbols[0].detail.as_deref(), Some("int"));
        assert_eq!(symbols[1].detail.as_deref(), Some("(n: int): int"));
    }

    #[test]
    fn document_symbols_list_an_enum_with_members() {
        let source = "\
module a

enum Status
    active
    archived
";
        let index = LineIndex::new(source);
        let symbols = document_symbols(&parse(source), &index);

        assert_eq!(symbols.len(), 1);
        let status = &symbols[0];
        assert_eq!(status.name, "Status");
        assert_eq!(status.kind, SymbolKind::ENUM);

        let children = status.children.as_ref().expect("enum has members");
        let names: Vec<&str> = children.iter().map(|child| child.name.as_str()).collect();
        assert_eq!(names, ["active", "archived"]);
        assert!(
            children
                .iter()
                .all(|child| child.kind == SymbolKind::ENUM_MEMBER)
        );
    }

    /// Analyze a one-file project on disk so the checked program has a module with
    /// real `source_file` paths for workspace symbols to locate.
    fn analyze(source: &str) -> CheckedProgram {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.mw"), source).unwrap();
        let config = parse_config(r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let snapshot = analyze_project(root, &config, &ProjectSources::new()).unwrap();
        std::mem::forget(dir);
        snapshot.program
    }

    #[test]
    fn workspace_symbols_list_functions_resources_and_constants() {
        let source = "\
module a

const LIMIT: int = 10

enum Status
    active
    archived

resource Book
    required title: string

store ^books(id: int): Book

pub fn add(title: string): Id(^books)
    var book: Book
    book.title = title
    return nextId(^books)
";
        let program = analyze(source);
        let symbols = workspace_symbols(&program);

        let by_name: std::collections::HashMap<&str, SymbolKind> =
            symbols.iter().map(|s| (s.name.as_str(), s.kind)).collect();
        assert_eq!(by_name.get("add"), Some(&SymbolKind::FUNCTION));
        assert_eq!(by_name.get("Book"), Some(&SymbolKind::STRUCT));
        assert_eq!(by_name.get("Status"), Some(&SymbolKind::ENUM));
        assert_eq!(by_name.get("LIMIT"), Some(&SymbolKind::CONSTANT));

        // Every symbol is located in module `a` by a `file://` URL.
        for symbol in &symbols {
            assert_eq!(symbol.container_name.as_deref(), Some("a"));
            assert_eq!(symbol.location.uri.scheme(), "file");
        }
    }
}
