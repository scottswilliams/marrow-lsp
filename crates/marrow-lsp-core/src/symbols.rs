//! Document and workspace symbols from parsed and checked project facts.
//!
//! Document symbols are the outline of one file: a [`DocumentSymbol`] per
//! declaration (constant, enum, resource, function), with enum members and resource
//! members nested beneath their parents. Workspace symbols flatten checked and
//! catalog-backed declarations into a project-wide list an editor's "go to symbol
//! in workspace" searches. Nothing here re-parses or re-checks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{DocumentSymbol, Location, SymbolInformation, SymbolKind, Url};
use marrow_check::{
    AnalysisSnapshot, CatalogDeclaration, CatalogEntryKind, CheckedFacts, EnumMemberFact,
    ModuleFact, ModuleId, ResourceMemberFact,
};
use marrow_syntax::{
    Declaration, EnumDecl, EnumMember, EvolveDecl, EvolveStep, FieldDecl, FunctionDecl, GroupDecl,
    IndexDecl, ResourceDecl, ResourceMember, SourceFile, SourceSpan, StoreDecl, SurfaceDecl,
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
        Declaration::Surface(surface) => surface_symbol(surface, index),
        Declaration::Evolve(evolve) => evolve_symbol(evolve, index),
    }
}

fn evolve_symbol(evolve: &EvolveDecl, index: &LineIndex) -> DocumentSymbol {
    let children = evolve
        .steps
        .iter()
        .map(|step| evolve_step_symbol(step, index))
        .collect();
    symbol(
        "evolve",
        None,
        SymbolKind::NAMESPACE,
        evolve.span,
        index,
        Some(children),
    )
}

fn evolve_step_symbol(step: &EvolveStep, index: &LineIndex) -> DocumentSymbol {
    let name = match step {
        EvolveStep::Rename { .. } => "rename",
        EvolveStep::Default { .. } => "default",
        EvolveStep::Retire { .. } => "retire",
        EvolveStep::Transform { .. } => "transform",
    };
    symbol(name, None, SymbolKind::EVENT, step.span(), index, None)
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

fn surface_symbol(surface: &SurfaceDecl, index: &LineIndex) -> DocumentSymbol {
    symbol(
        &surface.name,
        Some(format!("^{}", surface.store.root)),
        SymbolKind::INTERFACE,
        surface.span,
        index,
        None,
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

/// Checked functions, constants, and catalog declarations as a flat,
/// project-wide list for "go to symbol in workspace".
pub fn workspace_symbols(snapshot: &AnalysisSnapshot) -> Vec<SymbolInformation> {
    let sources = WorkspaceSources::new(snapshot);
    let containers = CatalogContainers::new(snapshot);
    let mut symbols = Vec::new();
    for module in &snapshot.program.modules {
        let Some(source) = sources.get(&module.source_file) else {
            continue;
        };
        for function in &module.functions {
            symbols.push(flat_symbol(
                &function.name,
                SymbolKind::FUNCTION,
                source,
                function.span,
                non_empty(&module.name),
            ));
        }
        for constant in &module.constants {
            symbols.push(flat_symbol(
                &constant.name,
                SymbolKind::CONSTANT,
                source,
                constant.span,
                non_empty(&module.name),
            ));
        }
    }
    for declaration in snapshot.catalog_declarations() {
        let Some(source) = sources.get(&declaration.file) else {
            continue;
        };
        let name = catalog_symbol_name(declaration);
        symbols.push(flat_symbol(
            &name,
            catalog_symbol_kind(declaration.kind),
            source,
            declaration.span,
            containers.get(declaration),
        ));
    }
    symbols
}

struct CatalogContainers {
    by_catalog_id: HashMap<String, String>,
}

impl CatalogContainers {
    fn new(snapshot: &AnalysisSnapshot) -> Self {
        let mut containers = Self {
            by_catalog_id: HashMap::new(),
        };
        let facts = &snapshot.program.facts;
        let modules = facts.modules();

        for declaration in snapshot.catalog_declarations() {
            if let Some(owner) = declaration_owner(facts, modules, declaration) {
                containers.insert_owner(declaration.catalog_id.as_str(), owner);
            }
        }

        containers
    }

    fn insert_owner(&mut self, catalog_id: &str, owner: String) {
        if !owner.is_empty() {
            self.by_catalog_id.insert(catalog_id.to_string(), owner);
        }
    }

    fn get(&self, declaration: &CatalogDeclaration) -> Option<&str> {
        self.by_catalog_id
            .get(&declaration.catalog_id)
            .map(String::as_str)
    }
}

fn module_name(modules: &[ModuleFact], id: ModuleId) -> &str {
    modules
        .get(id.0 as usize)
        .map_or("", |module| module.name.as_str())
}

fn declaration_owner(
    facts: &CheckedFacts,
    modules: &[ModuleFact],
    declaration: &CatalogDeclaration,
) -> Option<String> {
    match declaration.kind {
        CatalogEntryKind::Resource => facts
            .resources()
            .iter()
            .find(|resource| {
                resource.name == declaration.name
                    && resource.name_span == declaration.span
                    && fact_file(modules, resource.module) == Some(declaration.file.as_path())
            })
            .map(|resource| module_name(modules, resource.module).to_string()),
        CatalogEntryKind::Store => facts
            .stores()
            .iter()
            .find(|store| {
                store.root == declaration.name
                    && store.name_span == declaration.span
                    && fact_file(modules, store.module) == Some(declaration.file.as_path())
            })
            .map(|store| module_name(modules, store.module).to_string()),
        CatalogEntryKind::StoreIndex => facts.store_indexes().iter().find_map(|index| {
            let store = facts.store(index.store);
            (index.name == declaration.name
                && index.name_span == declaration.span
                && fact_file(modules, store.module) == Some(declaration.file.as_path()))
            .then(|| {
                let module = module_name(modules, store.module);
                join_owner(module, vec![format!("^{}", store.root)])
            })
        }),
        CatalogEntryKind::ResourceMember => facts.resource_members().iter().find_map(|member| {
            let resource = facts.resource(member.resource);
            (member.name == declaration.name
                && member.name_span == declaration.span
                && fact_file(modules, resource.module) == Some(declaration.file.as_path()))
            .then(|| {
                let module = module_name(modules, resource.module);
                join_owner(module, resource_member_owner_parts(facts, member))
            })
        }),
        CatalogEntryKind::Enum => facts
            .enums()
            .iter()
            .find(|enum_fact| {
                enum_fact.name == declaration.name
                    && enum_fact.name_span == declaration.span
                    && fact_file(modules, enum_fact.module) == Some(declaration.file.as_path())
            })
            .map(|enum_fact| module_name(modules, enum_fact.module).to_string()),
        CatalogEntryKind::EnumMember => facts.enum_members().iter().find_map(|member| {
            let enum_fact = facts.enum_(member.enum_id)?;
            (member.name == declaration.name
                && member.name_span == declaration.span
                && fact_file(modules, enum_fact.module) == Some(declaration.file.as_path()))
            .then(|| {
                let module = module_name(modules, enum_fact.module);
                join_owner(module, enum_member_owner_parts(facts, member))
            })
        }),
    }
}

fn fact_file(modules: &[ModuleFact], id: ModuleId) -> Option<&Path> {
    modules
        .get(id.0 as usize)
        .map(|module| module.source_file.as_path())
}

fn resource_member_owner_parts(facts: &CheckedFacts, member: &ResourceMemberFact) -> Vec<String> {
    let resource = facts.resource(member.resource);
    let mut parts = vec![resource.name.clone()];
    let mut parents = resource_member_parent_names(facts, member);
    parts.append(&mut parents);
    parts
}

fn resource_member_parent_names(facts: &CheckedFacts, member: &ResourceMemberFact) -> Vec<String> {
    let mut names = Vec::new();
    let mut parent = member.parent;
    while let Some(parent_id) = parent {
        let Some(parent_member) = facts.resource_members().get(parent_id.0 as usize) else {
            break;
        };
        names.push(parent_member.name.clone());
        parent = parent_member.parent;
    }
    names.reverse();
    names
}

fn enum_member_owner_parts(facts: &CheckedFacts, member: &EnumMemberFact) -> Vec<String> {
    let Some(enum_fact) = facts.enum_(member.enum_id) else {
        return Vec::new();
    };
    let mut parts = vec![enum_fact.name.clone()];
    let mut parents = enum_member_parent_names(facts, member);
    parts.append(&mut parents);
    parts
}

fn enum_member_parent_names(facts: &CheckedFacts, member: &EnumMemberFact) -> Vec<String> {
    let mut names = Vec::new();
    let mut parent = member.parent;
    while let Some(parent_id) = parent {
        let Some(parent_member) = facts.enum_member(parent_id) else {
            break;
        };
        names.push(parent_member.name.clone());
        parent = parent_member.parent;
    }
    names.reverse();
    names
}

fn join_owner(module: &str, parts: Vec<String>) -> String {
    let mut owner = String::new();
    if !module.is_empty() {
        owner.push_str(module);
    }
    for part in parts {
        if !owner.is_empty() {
            owner.push_str("::");
        }
        owner.push_str(&part);
    }
    owner
}

fn non_empty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
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

fn catalog_symbol_name(declaration: &CatalogDeclaration) -> String {
    match declaration.kind {
        CatalogEntryKind::Store => format!("^{}", declaration.name),
        _ => declaration.name.clone(),
    }
}

fn catalog_symbol_kind(kind: CatalogEntryKind) -> SymbolKind {
    match kind {
        CatalogEntryKind::Resource => SymbolKind::STRUCT,
        CatalogEntryKind::Store => SymbolKind::OBJECT,
        CatalogEntryKind::StoreIndex => SymbolKind::KEY,
        CatalogEntryKind::ResourceMember => SymbolKind::FIELD,
        CatalogEntryKind::Enum => SymbolKind::ENUM,
        CatalogEntryKind::EnumMember => SymbolKind::ENUM_MEMBER,
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
        let symbols = workspace_symbols(&snapshot);

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
        let symbols = workspace_symbols(&snapshot);
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
        let symbols = workspace_symbols(&snapshot);

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
        let symbols = workspace_symbols(&snapshot);

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
}
