//! Document and workspace symbols from parsed and checked project facts.
//!
//! Document symbols are the outline of one file: top-level declarations with
//! enum members, resource members, store indexes, and evolve steps nested beneath
//! their parents. Workspace symbols flatten checked and catalog-backed
//! declarations into a project-wide list an editor's "go to symbol in workspace"
//! searches. Nothing here re-parses or re-checks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{DocumentSymbol, Location, SymbolInformation, SymbolKind, Url};
use marrow_check::AnalysisSnapshot;
use marrow_check::tooling::{
    DocumentSymbol as MarrowDocumentSymbol, DocumentSymbolKind, SourceSymbolKind,
    document_symbols as marrow_document_symbols, source_symbols_matching,
};
use marrow_syntax::{SourceFile, SourceSpan};

use crate::positions::LineIndex;

/// The outline of one file: a symbol per top-level declaration, in source order.
///
/// `index` supplies the file's text and position mapping. A symbol's `range`
/// covers its whole declaration; its `selection_range` covers just the name, the
/// span an editor highlights and reveals when the symbol is chosen.
pub fn document_symbols(file: &SourceFile, index: &LineIndex) -> Vec<DocumentSymbol> {
    marrow_document_symbols(file, index.text())
        .into_iter()
        .map(|symbol| document_symbol_from_fact(symbol, index))
        .collect()
}

fn document_symbol_from_fact(symbol: MarrowDocumentSymbol, index: &LineIndex) -> DocumentSymbol {
    let children = (!symbol.children.is_empty()).then(|| {
        symbol
            .children
            .into_iter()
            .map(|child| document_symbol_from_fact(child, index))
            .collect()
    });
    #[allow(deprecated)]
    DocumentSymbol {
        name: symbol.name,
        detail: symbol.detail,
        kind: document_symbol_kind(symbol.kind),
        tags: None,
        deprecated: None,
        range: index.range(symbol.span.start_byte, symbol.span.end_byte),
        selection_range: index.range(
            symbol.selection_span.start_byte,
            symbol.selection_span.end_byte,
        ),
        children,
    }
}

fn document_symbol_kind(kind: DocumentSymbolKind) -> SymbolKind {
    match kind {
        DocumentSymbolKind::Constant => SymbolKind::CONSTANT,
        DocumentSymbolKind::Function => SymbolKind::FUNCTION,
        DocumentSymbolKind::Enum => SymbolKind::ENUM,
        DocumentSymbolKind::EnumMember => SymbolKind::ENUM_MEMBER,
        DocumentSymbolKind::Resource => SymbolKind::STRUCT,
        DocumentSymbolKind::ResourceField => SymbolKind::FIELD,
        DocumentSymbolKind::ResourceGroup => SymbolKind::OBJECT,
        DocumentSymbolKind::Store => SymbolKind::OBJECT,
        DocumentSymbolKind::StoreIndex => SymbolKind::KEY,
        DocumentSymbolKind::Surface => SymbolKind::INTERFACE,
        DocumentSymbolKind::Evolve => SymbolKind::NAMESPACE,
        DocumentSymbolKind::EvolveStep => SymbolKind::EVENT,
    }
}

/// Checked functions, constants, and catalog declarations as a flat,
/// project-wide list for "go to symbol in workspace".
pub fn workspace_symbols(snapshot: &AnalysisSnapshot, search_text: &str) -> Vec<SymbolInformation> {
    let sources = WorkspaceSources::new(snapshot);
    source_symbols_matching(snapshot, search_text)
        .into_iter()
        .filter_map(|symbol| {
            let source = sources.get(&symbol.file)?;
            Some(flat_symbol(
                &symbol.name,
                source_symbol_kind(symbol.kind),
                source,
                symbol.span,
                symbol.container.as_deref(),
            ))
        })
        .collect()
}

struct WorkspaceSources {
    by_file: HashMap<PathBuf, WorkspaceSource>,
}

impl WorkspaceSources {
    fn new(snapshot: &AnalysisSnapshot) -> Self {
        let by_file = snapshot
            .files
            .iter()
            .filter_map(|file| {
                let url = Url::from_file_path(&file.path).ok()?;
                Some((
                    file.path.clone(),
                    WorkspaceSource {
                        url,
                        index: LineIndex::new(file.source.clone()),
                    },
                ))
            })
            .collect();
        Self { by_file }
    }

    fn get(&self, file: &Path) -> Option<&WorkspaceSource> {
        self.by_file.get(file)
    }
}

struct WorkspaceSource {
    url: Url,
    index: LineIndex,
}

fn source_symbol_kind(kind: SourceSymbolKind) -> SymbolKind {
    match kind {
        SourceSymbolKind::Constant => SymbolKind::CONSTANT,
        SourceSymbolKind::Function => SymbolKind::FUNCTION,
        SourceSymbolKind::Resource => SymbolKind::STRUCT,
        SourceSymbolKind::Store => SymbolKind::OBJECT,
        SourceSymbolKind::StoreIndex => SymbolKind::KEY,
        SourceSymbolKind::ResourceMember => SymbolKind::FIELD,
        SourceSymbolKind::Enum => SymbolKind::ENUM,
        SourceSymbolKind::EnumMember => SymbolKind::ENUM_MEMBER,
    }
}

fn flat_symbol(
    name: &str,
    kind: SymbolKind,
    source: &WorkspaceSource,
    span: SourceSpan,
    container: Option<&str>,
) -> SymbolInformation {
    #[allow(deprecated)]
    SymbolInformation {
        name: name.to_string(),
        kind,
        tags: None,
        deprecated: None,
        location: Location {
            uri: source.url.clone(),
            range: source.index.range(span.start_byte, span.end_byte),
        },
        container_name: container.map(ToString::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_check::{AnalysisSnapshot, ProjectSources, analyze_project};
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

    #[test]
    fn document_symbols_list_evolve_steps_as_source_outline_children() {
        let source = "\
module a

resource Book
    required title: string

store ^books(id: int): Book

evolve
    rename Book.title -> Book.name
    default Book.name = \"untitled\"
    retire Book.title
    transform ^books
        const id: Id(^books) = 1
";
        let index = LineIndex::new(source);
        let symbols = document_symbols(&parse(source), &index);

        let evolve = symbols
            .iter()
            .find(|symbol| symbol.name == "evolve")
            .expect("evolve block is listed");
        assert_eq!(evolve.kind, SymbolKind::NAMESPACE);

        let children = evolve.children.as_ref().expect("evolve lists steps");
        let names: Vec<&str> = children.iter().map(|child| child.name.as_str()).collect();
        assert_eq!(names, ["rename", "default", "retire", "transform"]);
        assert!(children.iter().all(|child| child.kind == SymbolKind::EVENT));
    }

    #[test]
    fn document_symbols_list_surface_declarations() {
        let source = "\
module shelf

surface Books from ^books
    fields title
";
        let index = LineIndex::new(source);
        let symbols = document_symbols(&parse(source), &index);

        let surface = symbols
            .iter()
            .find(|symbol| symbol.name == "Books")
            .expect("surface declaration is listed");
        assert_eq!(surface.kind, SymbolKind::INTERFACE);
        assert_eq!(surface.detail.as_deref(), Some("^books"));
    }

    #[test]
    fn document_symbols_do_not_own_outline_assembly() {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("symbols.rs"),
        )
        .expect("read symbols source");
        let production = source.split("#[cfg(test)]").next().unwrap_or(&source);

        for obsolete in [
            "fn declaration_symbol",
            "fn evolve_symbol",
            "fn enum_symbol",
            "fn resource_symbol",
            "fn store_symbol",
            "fn surface_symbol",
            "fn function_signature",
            "fn name_span",
            "Declaration::",
            "ResourceMember::",
        ] {
            assert!(
                !production.contains(obsolete),
                "document symbols should consume marrow-check outline facts, not own `{obsolete}`"
            );
        }
    }

    /// Analyze a one-file project through the production project pipeline.
    fn analyze(source: &str) -> AnalysisSnapshot {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.mw"), source).unwrap();
        let config =
            parse_config(r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#)
                .unwrap();
        analyze_project(root, &config, &ProjectSources::new(), None, None).unwrap()
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
    index byTitle(title, id)

pub fn add(title: string): Id(^books)
    var book: Book
    book.title = title
    return nextId(^books)
";
        let snapshot = analyze(source);
        let symbols = workspace_symbols(&snapshot, "");

        let by_name: std::collections::HashMap<&str, SymbolKind> =
            symbols.iter().map(|s| (s.name.as_str(), s.kind)).collect();
        assert_eq!(by_name.get("add"), Some(&SymbolKind::FUNCTION));
        assert_eq!(by_name.get("Book"), Some(&SymbolKind::STRUCT));
        assert_eq!(by_name.get("Status"), Some(&SymbolKind::ENUM));
        assert_eq!(by_name.get("LIMIT"), Some(&SymbolKind::CONSTANT));
        assert_eq!(by_name.get("^books"), Some(&SymbolKind::OBJECT));
        assert_eq!(by_name.get("byTitle"), Some(&SymbolKind::KEY));
        assert_eq!(by_name.get("active"), Some(&SymbolKind::ENUM_MEMBER));

        for symbol in &symbols {
            assert_eq!(symbol.location.uri.scheme(), "file");
        }
        for name in ["add", "Book", "Status", "LIMIT", "^books"] {
            let symbol = symbols
                .iter()
                .find(|symbol| symbol.name == name)
                .unwrap_or_else(|| panic!("{name} symbol"));
            assert_eq!(symbol.container_name.as_deref(), Some("a"));
        }
        let index = symbols
            .iter()
            .find(|symbol| symbol.name == "byTitle")
            .expect("index symbol");
        assert_eq!(index.container_name.as_deref(), Some("a::^books"));
        let member = symbols
            .iter()
            .find(|symbol| symbol.name == "active")
            .expect("enum member symbol");
        assert_eq!(member.container_name.as_deref(), Some("a::Status"));
    }

    #[test]
    fn workspace_symbols_use_analyzed_source_positions() {
        let source = "\
module a

resource Book
    required title: string

store ^books(id: int): Book
    index byTitle(title, id)

enum Status
    active

pub fn add(title: string): Id(^books)
    return nextId(^books)
";
        let snapshot = analyze(source);
        let symbols = workspace_symbols(&snapshot, "");
        let book = symbols
            .iter()
            .find(|symbol| symbol.name == "Book")
            .expect("Book resource symbol");

        assert_eq!(book.location.range.start.line, 2);
        assert_eq!(
            book.location.range.start.character,
            "resource ".len() as u32
        );
    }

    #[test]
    fn workspace_symbols_do_not_invent_script_containers() {
        let source = "\
resource Book
    required title: string

store ^books(id: int): Book

fn add(): Id(^books)
    return nextId(^books)
";
        let snapshot = analyze(source);
        let symbols = workspace_symbols(&snapshot, "");

        let add = symbols
            .iter()
            .find(|symbol| symbol.name == "add")
            .expect("function symbol");
        let book = symbols
            .iter()
            .find(|symbol| symbol.name == "Book")
            .expect("resource symbol");
        let store = symbols
            .iter()
            .find(|symbol| symbol.name == "^books")
            .expect("store symbol");

        assert_eq!(add.container_name, None);
        assert_eq!(book.container_name, None);
        assert_eq!(store.container_name, None);
    }

    #[test]
    fn workspace_symbols_label_nested_catalog_owners() {
        let source = "\
module a

resource Book
    required title: string

resource Movie
    required title: string

store ^books(id: int): Book
    index byTitle(title, id)

enum Status
    active
";
        let snapshot = analyze(source);
        let symbols = workspace_symbols(&snapshot, "");

        let mut title_containers: Vec<&str> = symbols
            .iter()
            .filter(|symbol| symbol.name == "title")
            .filter_map(|symbol| symbol.container_name.as_deref())
            .collect();
        title_containers.sort_unstable();
        assert_eq!(title_containers, ["a::Book", "a::Movie"]);

        let index = symbols
            .iter()
            .find(|symbol| symbol.name == "byTitle")
            .expect("store index symbol");
        assert_eq!(index.container_name.as_deref(), Some("a::^books"));

        let member = symbols
            .iter()
            .find(|symbol| symbol.name == "active")
            .expect("enum member symbol");
        assert_eq!(member.container_name.as_deref(), Some("a::Status"));
    }

    #[test]
    fn workspace_symbols_delegate_search_ranking_to_marrow() {
        let source = "\
module a

const bookValue: int = 1

resource Book
    required title: string
";
        let snapshot = analyze(source);
        let symbols = workspace_symbols(&snapshot, "book");

        let observed: Vec<(&str, Option<&str>)> = symbols
            .iter()
            .map(|symbol| (symbol.name.as_str(), symbol.container_name.as_deref()))
            .collect();
        assert_eq!(
            observed,
            [
                ("Book", Some("a")),
                ("bookValue", Some("a")),
                ("title", Some("a::Book")),
            ]
        );
    }

    #[test]
    fn workspace_symbols_do_not_own_catalog_classification() {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("symbols.rs"),
        )
        .expect("read symbols source");
        let production = source.split("#[cfg(test)]").next().unwrap_or(&source);

        for obsolete in [
            "CatalogEntryKind",
            "struct CatalogContainers",
            "fn declaration_owner",
            "fn catalog_symbol_kind",
            "fn source_symbol_match",
            "sort_by_key",
        ] {
            assert!(
                !production.contains(obsolete),
                "workspace symbols should consume marrow-check source-symbol facts, not own `{obsolete}`"
            );
        }
    }
}
