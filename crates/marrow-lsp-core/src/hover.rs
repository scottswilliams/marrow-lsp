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

/// The hover for byte `offset` in `file`, or `None` when no type covers it.
///
/// The file must be one the snapshot analyzed; a path it does not know (or an
/// offset no expression covers) yields `None`, so the editor shows nothing.
pub fn hover(snapshot: &AnalysisSnapshot, file: &Path, offset: usize) -> Option<Hover> {
    let analyzed = snapshot.files.iter().find(|f| f.path == file)?;
    let ty = type_at(&snapshot.program, file, &analyzed.parsed, offset)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: marrow_code_block(&render_type(&ty)),
        }),
        range: None,
    })
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
}
