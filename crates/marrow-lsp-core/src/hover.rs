//! Hover: the type of the expression under the cursor, as a `marrow` code block.
//!
//! An editor sends a byte offset (already converted from an LSP position by the
//! document's [`LineIndex`](crate::positions::LineIndex)); this asks the checker
//! for the type there with [`marrow_check::type_at`] and renders it as a fenced
//! `marrow` block so the client shows it with `.mw` syntax highlighting. The type
//! comes only from the checker — this module never re-infers anything.

use std::path::Path;

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};
use marrow_check::{AnalysisSnapshot, BindingIndex, SymbolKind, SymbolRef, type_at};
use marrow_store::backend::Presence;
use marrow_syntax::{Declaration, EnumDecl, EnumMember, ResourceDecl, ResourceMember, SourceSpan};

use crate::store::{Availability, StoreReader, StoredValue, saved_path_at};
use crate::types::render_type;

/// The hover for byte `offset` in `file`, or `None` when no type covers it.
///
/// The file must be one the snapshot analyzed; a path it does not know (or an
/// offset no expression covers) yields `None`, so the editor shows nothing.
pub fn hover(snapshot: &AnalysisSnapshot, file: &Path, offset: usize) -> Option<Hover> {
    hover_with_live(snapshot, file, offset, None, None)
}

/// The hover for byte `offset`, augmented with the symbol's documentation and the
/// live stored value when each is available.
///
/// The type always comes from the checker and is shown first. `docs`, when
/// present, is the symbol's `;;` description rendered as a paragraph below the
/// type. When `reader` is `Some` and the cursor resolves to a concrete saved path,
/// a final line shows the live value (a typed scalar, `N records`, `present`, or
/// `absent`). A store that is `Unavailable` (locked, corrupt, missing) adds
/// nothing — the hover shows only the type and docs, never an error. Passing
/// `None` for `reader` disables live data entirely (the `marrow.liveData` setting
/// is off, or no native store is configured); passing `None` for `docs` omits the
/// description (the symbol has none, or is a local/parameter that carries none).
pub fn hover_with_live(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    docs: Option<&str>,
    reader: Option<&StoreReader>,
) -> Option<Hover> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let ty = type_at(&snapshot.program, file, &analyzed.parsed, offset)?;
    let mut value = marrow_code_block(&render_type(&ty));

    // The documentation paragraph sits between the type and any live value: the
    // type is the primary fact, the docs explain the symbol, the live value is
    // the current store reading.
    if let Some(docs) = docs {
        value.push_str("\n\n");
        value.push_str(docs);
    }

    if let Some(reader) = reader
        && let Some(segments) = saved_path_at(&analyzed.parsed.file, &snapshot.program, offset)
        && let Some(line) = live_value_line(reader, &segments, &snapshot.program)
    {
        value.push_str("\n\n");
        value.push_str(&line);
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    })
}

/// The `;;` description of the symbol at byte `offset` in `file`, as a Markdown
/// paragraph, or `None` when none applies.
///
/// The symbol is resolved through the cached [`BindingIndex`] (the same one
/// go-to-definition and references read), so this never rebuilds resolution per
/// request. Only documentable declarations — functions, resources, module
/// constants, enums, enum members, and a resource's saved fields, layers, and
/// indexes — carry docs; a local or parameter has none and yields `None`. The
/// declaration is located in
/// the snapshot's parsed file by matching the definition span, and its `docs`
/// lines (each a `;;` line) are joined into one paragraph. An undocumented
/// declaration yields `None`.
pub fn symbol_docs(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<String> {
    let symbol = index.definition(file, offset)?;
    let analyzed = snapshot.files.iter().find(|f| f.path == symbol.file)?;
    let docs = declaration_docs(&analyzed.parsed.file, &symbol)?;
    join_docs(docs)
}

/// The `docs` lines of the declaration the definition `symbol` names, found in
/// `source` by matching the definition span, or `None` when the symbol is not a
/// documentable declaration. A resource's generated identity shares its
/// resource's span, so it reads the resource's docs; locals and parameters are
/// not declarations here and return `None`.
fn declaration_docs<'a>(
    source: &'a marrow_syntax::SourceFile,
    symbol: &SymbolRef,
) -> Option<&'a [String]> {
    match symbol.kind {
        SymbolKind::Function => {
            source
                .declarations
                .iter()
                .find_map(|declaration| match declaration {
                    Declaration::Function(function) if function.span == symbol.span => {
                        Some(function.docs.as_slice())
                    }
                    _ => None,
                })
        }
        SymbolKind::ModuleConst => {
            source
                .declarations
                .iter()
                .find_map(|declaration| match declaration {
                    Declaration::Const(constant) if constant.span == symbol.span => {
                        Some(constant.docs.as_slice())
                    }
                    _ => None,
                })
        }
        SymbolKind::Resource | SymbolKind::ResourceIdentity => {
            resource_at(source, symbol.span).map(|resource| resource.docs.as_slice())
        }
        SymbolKind::Enum => enum_at(source, symbol.span).map(|enum_decl| enum_decl.docs.as_slice()),
        SymbolKind::EnumMember => {
            source
                .declarations
                .iter()
                .find_map(|declaration| match declaration {
                    Declaration::Enum(enum_decl) => {
                        enum_member_docs(&enum_decl.members, symbol.span)
                    }
                    _ => None,
                })
        }
        SymbolKind::Field | SymbolKind::Layer | SymbolKind::Index => source
            .declarations
            .iter()
            .find_map(|declaration| match declaration {
                Declaration::Resource(resource) => member_docs(&resource.members, symbol.span),
                _ => None,
            }),
        // Locals, parameters, and module aliases carry no `;;` documentation.
        SymbolKind::Local | SymbolKind::Param | SymbolKind::ModuleRef => None,
    }
}

/// The resource declaration whose span is `span`, or `None`.
fn resource_at(source: &marrow_syntax::SourceFile, span: SourceSpan) -> Option<&ResourceDecl> {
    source
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Resource(resource) if resource.span == span => Some(resource),
            _ => None,
        })
}

/// The enum declaration whose span is `span`, or `None`.
fn enum_at(source: &marrow_syntax::SourceFile, span: SourceSpan) -> Option<&EnumDecl> {
    source
        .declarations
        .iter()
        .find_map(|declaration| match declaration {
            Declaration::Enum(enum_decl) if enum_decl.span == span => Some(enum_decl),
            _ => None,
        })
}

/// The `docs` of the enum member whose span is `span`, searching nested members,
/// or `None`.
fn enum_member_docs(members: &[EnumMember], span: SourceSpan) -> Option<&[String]> {
    for member in members {
        if member.span == span {
            return Some(&member.docs);
        }
        if let Some(docs) = enum_member_docs(&member.members, span) {
            return Some(docs);
        }
    }
    None
}

/// The `docs` of the resource member (field, group/layer, or index) whose span is
/// `span`, searching nested groups, or `None`.
fn member_docs(members: &[ResourceMember], span: SourceSpan) -> Option<&[String]> {
    for member in members {
        match member {
            ResourceMember::Field(field) if field.span == span => return Some(&field.docs),
            ResourceMember::Group(group) if group.span == span => return Some(&group.docs),
            ResourceMember::Index(index) if index.span == span => return Some(&index.docs),
            // A nested group can hold the named member; descend before moving on.
            ResourceMember::Group(group) => {
                if let Some(docs) = member_docs(&group.members, span) {
                    return Some(docs);
                }
            }
            _ => {}
        }
    }
    None
}

/// Join `;;` doc lines into one Markdown paragraph, or `None` when there is no doc
/// text (no lines, or only blank ones).
fn join_docs(lines: &[String]) -> Option<String> {
    let joined = lines
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let joined = joined.trim();
    if joined.is_empty() {
        None
    } else {
        Some(joined.to_string())
    }
}

/// The live-data line for a resolved saved path, or `None` when the store is
/// unavailable so the hover stays type-only. A root path shows its record count;
/// any other path shows its presence and, for a stored value, the typed value.
fn live_value_line(
    reader: &StoreReader,
    segments: &[marrow_store::path::PathSegment],
    program: &marrow_check::CheckedProgram,
) -> Option<String> {
    use marrow_store::path::PathSegment;
    // A bare `^root` reads as the record count, the useful fact at a root.
    if let [PathSegment::Root(root)] = segments {
        return match reader.record_count(root) {
            Availability::Available(count) => Some(format!("**live**: {}", count.display())),
            Availability::Unavailable => None,
        };
    }
    match reader.get(segments, program) {
        Availability::Available(stored) => Some(format!("**live**: {}", present(&stored))),
        Availability::Unavailable => None,
    }
}

/// The live-value text for a non-root path: the stored value when one is present,
/// else a presence word.
fn present(stored: &StoredValue) -> String {
    match (&stored.value, stored.presence) {
        (Some(value), _) => value.clone(),
        (None, Presence::ChildrenOnly | Presence::ValueAndChildren) => "present".to_string(),
        (None, Presence::Absent | Presence::ValueOnly) => "absent".to_string(),
    }
}

/// Wrap `code` in a fenced `marrow` block so the client renders it as `.mw`.
fn marrow_code_block(code: &str) -> String {
    format!("```marrow\n{code}\n```")
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_check::{ProjectSources, analyze_project};
    use marrow_project::parse_config;

    /// Analyze a one-file project laid out on disk under a temp dir, returning the
    /// snapshot and the module file's path so a test can hover into it.
    fn analyze(source: &str) -> (AnalysisSnapshot, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("a.mw");
        std::fs::write(&file, source).unwrap();
        let config = parse_config(r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let snapshot = analyze_project(root, &config, &ProjectSources::new()).unwrap();
        // Keep the temp dir alive for the snapshot's lifetime by leaking it; the
        // OS reclaims it when the test process exits.
        std::mem::forget(dir);
        (snapshot, file)
    }

    /// The byte offset of the first occurrence of `needle` in `source`.
    fn offset_of(source: &str, needle: &str) -> usize {
        source.find(needle).expect("needle present in source")
    }

    #[test]
    fn hover_renders_a_parameters_type_as_a_marrow_block() {
        let source = "module a\n\npub fn f(n: int): int\n    return n\n";
        let (snapshot, file) = analyze(source);
        // Hover over the `n` in `return n`.
        let offset = offset_of(source, "return n") + "return ".len();
        let hover = hover(&snapshot, &file, offset).expect("a type at the use of `n`");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert_eq!(markup.value, "```marrow\nint\n```");
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

    /// Analyze a project that pins a native store and seed `^books(1).title =
    /// "Mort"`. Returns the snapshot, the module file path, the resolved project,
    /// and the live source text so a test can hover into a saved path.
    fn analyze_with_store(
        source: &str,
    ) -> (
        AnalysisSnapshot,
        std::path::PathBuf,
        crate::workspace::Project,
    ) {
        use marrow_store::backend::Backend;
        use marrow_store::path::{PathSegment, SavedKey, encode_path};
        use marrow_store::redb::RedbStore;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let config_text = r#"{
            "sourceRoots": ["src"],
            "store": { "backend": "native", "dataDir": "data" }
        }"#;
        std::fs::write(root.join("marrow.json"), config_text).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        let file = src.join("a.mw");
        std::fs::write(&file, source).unwrap();

        let store_path = root.join("data").join("marrow.redb");
        {
            let mut store = RedbStore::open(&store_path).unwrap();
            let title = encode_path(&[
                PathSegment::Root("books".to_string()),
                PathSegment::RecordKey(SavedKey::Int(1)),
                PathSegment::Field("title".to_string()),
            ]);
            store.write(&title, b"Mort".to_vec()).unwrap();
        }

        let config = parse_config(config_text).unwrap();
        let snapshot = analyze_project(root, &config, &ProjectSources::new()).unwrap();
        let project = crate::workspace::Project {
            root: root.to_path_buf(),
            config,
        };
        std::mem::forget(dir);
        (snapshot, file, project)
    }

    #[test]
    fn hover_on_a_saved_path_shows_the_live_value() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

pub fn f(): string
    return ^books(1).title
";
        let (snapshot, file, project) = analyze_with_store(source);
        let reader = StoreReader::for_project(&project).expect("native store reader");

        // Hover over `title` in the function body's `^books(1).title`.
        let offset = source.rfind(").title").unwrap() + ").".len() + 1;
        let hover = hover_with_live(&snapshot, &file, offset, None, Some(&reader))
            .expect("a hover at the path");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup contents");
        };
        assert!(
            markup.value.contains("```marrow\nstring\n```"),
            "the type is always shown: {}",
            markup.value
        );
        assert!(
            markup.value.contains("\"Mort\""),
            "the live stored value should be shown: {}",
            markup.value
        );
    }

    #[test]
    fn hover_on_a_saved_root_shows_the_record_count() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

pub fn f()
    delete ^books
";
        let (snapshot, file, project) = analyze_with_store(source);
        let reader = StoreReader::for_project(&project).unwrap();
        // Hover over `^books` in the body. The type at a bare root may not resolve,
        // so this asserts the path resolver + count only when a hover is produced.
        let offset = source.rfind("^books").unwrap();
        if let Some(hover) = hover_with_live(&snapshot, &file, offset, None, Some(&reader)) {
            let HoverContents::Markup(markup) = hover.contents else {
                panic!("expected markup");
            };
            assert!(
                markup.value.contains("1 record"),
                "a root hover should show the record count: {}",
                markup.value
            );
        }
    }

    #[test]
    fn hover_without_a_reader_is_type_only() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

pub fn f(): string
    return ^books(1).title
";
        let (snapshot, file, _project) = analyze_with_store(source);
        let offset = source.rfind(").title").unwrap() + ").".len() + 1;
        // No reader, no docs: the live-data setting is off. Only the type shows.
        let hover = hover_with_live(&snapshot, &file, offset, None, None).expect("a hover");
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
        let hover = hover_with_live(&snapshot, &file, offset, docs.as_deref(), None)
            .expect("a hover at the call");
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
    fn symbol_docs_for_a_resource_name_returns_its_description() {
        let source = "\
module a

;; A book on a shelf.
resource Book at ^books(id: int)
    required title: string

pub fn f(): Book
    return Book(id: 1, title: \"x\")
";
        let (snapshot, file) = analyze(source);
        let index = index_for(&snapshot);
        // Hover over the constructor use `Book(...)`.
        let offset = source.rfind("Book(id: 1").unwrap();
        let docs = symbol_docs(&snapshot, &index, &file, offset).expect("resource docs");
        assert_eq!(docs, "A book on a shelf.");
    }

    #[test]
    fn symbol_docs_for_a_saved_field_returns_its_description() {
        let source = "\
module a

resource Book at ^books(id: int)
    ;; The book's title.
    required title: string

pub fn f(): string
    return ^books(1).title
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
}
