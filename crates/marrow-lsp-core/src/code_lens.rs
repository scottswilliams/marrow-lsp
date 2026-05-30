//! Record-count CodeLens for saved resources.
//!
//! A resource attached to a saved root gets a lens over its declaration showing
//! how many records live under that root. The count is a live store read, so it
//! is computed lazily on `codeLens/resolve` rather than during the initial
//! `textDocument/codeLens` pass — the editor asks for the count only for the
//! lenses actually on screen, and an offscreen resource costs nothing. A store
//! that cannot be read right now yields no resolved command, so the lens quietly
//! disappears rather than showing an error.

use lsp_types::{CodeLens, Command, Range};

use crate::positions::LineIndex;
use crate::store::{Availability, StoreReader};

/// The unresolved record-count lenses for one file: one per resource declaration
/// that is attached to a saved root. Each lens carries its root name in `data` so
/// the resolve step knows which root to count without re-parsing.
///
/// The lenses are unresolved — their `command` is `None` — because the count is a
/// live read. `index` maps the resource declaration's span to a one-line range
/// the editor anchors the lens to.
pub fn code_lenses(file: &marrow_syntax::SourceFile, index: &LineIndex) -> Vec<CodeLens> {
    file.declarations
        .iter()
        .filter_map(|declaration| match declaration {
            marrow_syntax::Declaration::Resource(resource) => {
                resource.store.as_ref().map(|store| {
                    let range = lens_range(resource.span, index);
                    CodeLens {
                        range,
                        command: None,
                        data: Some(serde_json::json!({ "root": store.root })),
                    }
                })
            }
            _ => None,
        })
        .collect()
}

/// Resolve a record-count lens: read the live count for the root named in its
/// `data` and set the lens command to display it. When the store is unavailable
/// or the lens carries no root, the lens resolves to a command-less placeholder so
/// the editor shows nothing rather than a stale or erroring count.
pub fn resolve(lens: CodeLens, reader: Option<&StoreReader>) -> CodeLens {
    let Some(root) = lens
        .data
        .as_ref()
        .and_then(|data| data.get("root")?.as_str())
    else {
        return lens;
    };
    let title = reader
        .map(|reader| reader.record_count(root))
        .and_then(Availability::into_option)
        .map(|count| format!("^{root} — {}", count.display()));

    match title {
        Some(title) => CodeLens {
            command: Some(Command {
                title,
                // A display-only lens: it carries no editor command to run.
                command: String::new(),
                arguments: None,
            }),
            ..lens
        },
        // Store unavailable: leave the lens command-less so it shows quietly.
        None => lens,
    }
}

/// The one-line range a resource's lens anchors to: the first line of its
/// declaration span. A lens should span a single line, so only the start line is
/// used.
fn lens_range(span: marrow_syntax::SourceSpan, index: &LineIndex) -> Range {
    let start = index.range(span.start_byte, span.start_byte).start;
    Range { start, end: start }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_syntax::parse_source;

    fn file(source: &str) -> marrow_syntax::SourceFile {
        parse_source(source).file
    }

    #[test]
    fn a_saved_resource_gets_one_lens_carrying_its_root() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

resource Local
    required note: string
";
        let index = LineIndex::new(source);
        let lenses = code_lenses(&file(source), &index);
        // Only the saved resource gets a lens; the unsaved `Local` does not.
        assert_eq!(lenses.len(), 1);
        let lens = &lenses[0];
        assert_eq!(lens.command, None, "the lens is unresolved");
        assert_eq!(lens.data.as_ref().unwrap()["root"], "books");
        // The lens anchors to the `resource Book` line (zero-based line 2).
        assert_eq!(lens.range.start.line, 2);
    }

    #[test]
    fn resolve_without_a_reader_leaves_the_lens_quiet() {
        let source = "module a\n\nresource Book at ^books(id: int)\n    required title: string\n";
        let index = LineIndex::new(source);
        let lens = code_lenses(&file(source), &index).remove(0);
        let resolved = resolve(lens, None);
        assert_eq!(
            resolved.command, None,
            "no store means no count, and a quiet (command-less) lens"
        );
    }
}
