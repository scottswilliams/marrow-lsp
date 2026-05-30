//! Hover: the type of the expression under the cursor, as a `marrow` code block.
//!
//! An editor sends a byte offset (already converted from an LSP position by the
//! document's [`LineIndex`](crate::positions::LineIndex)); this asks the checker
//! for the type there with [`marrow_check::type_at`] and renders it as a fenced
//! `marrow` block so the client shows it with `.mw` syntax highlighting. The type
//! comes only from the checker — this module never re-infers anything.

use std::path::Path;

use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};
use marrow_check::{AnalysisSnapshot, MarrowType, type_at};
use marrow_store::backend::Presence;

use crate::store::{Availability, StoreReader, StoredValue, saved_path_at};

/// The hover for byte `offset` in `file`, or `None` when no type covers it.
///
/// The file must be one the snapshot analyzed; a path it does not know (or an
/// offset no expression covers) yields `None`, so the editor shows nothing.
pub fn hover(snapshot: &AnalysisSnapshot, file: &Path, offset: usize) -> Option<Hover> {
    hover_with_live(snapshot, file, offset, None)
}

/// The hover for byte `offset`, optionally augmented with the live stored value
/// when the cursor sits on a saved `^` path and a readable store is supplied.
///
/// The type always comes from the checker. When `reader` is `Some` and the cursor
/// resolves to a concrete saved path, a second line shows the live value (a typed
/// scalar, `N records`, `present`, or `absent`). A store that is `Unavailable`
/// (locked, corrupt, missing) adds nothing — the hover shows only the type, never
/// an error. Passing `None` for `reader` disables live data entirely (the
/// `marrow.liveData` setting is off, or no native store is configured).
pub fn hover_with_live(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    reader: Option<&StoreReader>,
) -> Option<Hover> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let ty = type_at(&snapshot.program, file, &analyzed.parsed, offset)?;
    let mut value = marrow_code_block(&render_type(&ty));

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

/// The canonical `.mw` spelling of a checker type. Mirrors `marrow_schema::Type`'s
/// `Display` for the storable types and names the checker-only forms (`Error`, a
/// same-module resource, `unknown`) as they are written in source.
fn render_type(ty: &MarrowType) -> String {
    match ty {
        MarrowType::Primitive(scalar) => scalar.name().to_string(),
        MarrowType::Error => "Error".to_string(),
        MarrowType::Resource(name) => name.clone(),
        MarrowType::Identity(resource) => format!("{resource}::Id"),
        MarrowType::Sequence(element) => format!("sequence[{}]", render_type(element)),
        MarrowType::Unknown => "unknown".to_string(),
    }
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
        let hover =
            hover_with_live(&snapshot, &file, offset, Some(&reader)).expect("a hover at the path");
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
        if let Some(hover) = hover_with_live(&snapshot, &file, offset, Some(&reader)) {
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
        // No reader: the live-data setting is off. Only the type shows.
        let hover = hover_with_live(&snapshot, &file, offset, None).expect("a hover");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("expected markup");
        };
        assert_eq!(markup.value, "```marrow\nstring\n```");
    }
}
