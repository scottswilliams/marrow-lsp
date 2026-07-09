//! Whole-document formatting via the canonical `marrow_syntax::format_source`.
//!
//! `textDocument/formatting` reformats a buffer in-process and returns a single
//! full-document [`TextEdit`] — but only when the buffer parses cleanly. The
//! formatter rebuilds source from the parse tree, so reformatting a file with a
//! parse error would silently drop the unparsed text; refusing to format an
//! erroring buffer keeps malformed source intact until the user fixes it.
//!
//! It reads the document's cached [`ParsedSource`] to decide cleanliness — never
//! re-parsing — and calls the formatter only on the text it already holds.

use lsp_types::{Position, Range, TextEdit};
use marrow_syntax::{ParsedSource, format_source};

use crate::positions::LineIndex;

/// The formatting edit for `text`, or `None` when the buffer must be left as-is.
///
/// Returns a single edit replacing the whole document with the canonical
/// formatting when `parsed` carries no errors and the result actually differs;
/// returns `None` when the buffer has a parse error (so malformed source is never
/// lossily rewritten) or is already canonical (so the editor records no change).
/// `index` maps the document's end to an LSP position for the replacement range.
pub fn formatting(text: &str, parsed: &ParsedSource, index: &LineIndex) -> Option<Vec<TextEdit>> {
    if parsed.has_errors() {
        return None;
    }
    let formatted = format_source(text);
    if formatted == text {
        return None;
    }
    let end = index.position(text.len());
    Some(vec![TextEdit {
        range: Range {
            start: Position::new(0, 0),
            end: Position::new(end.line, end.character),
        },
        new_text: formatted,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_syntax::parse_source;

    #[test]
    fn a_clean_but_unformatted_buffer_is_reformatted_in_full() {
        // Extra blank lines and ragged spacing: parses cleanly, not canonical.
        let source = "module a\n\n\n\nconst   N:int=1\n";
        let parsed = parse_source(source);
        let index = LineIndex::new(source);
        let edits = formatting(source, &parsed, &index).expect("a clean buffer is formatted");
        assert_eq!(edits.len(), 1, "a single whole-document edit");
        let edit = &edits[0];
        // The edit spans the whole document, start to end.
        assert_eq!(edit.range.start, Position::new(0, 0));
        // Its text is exactly what the canonical formatter produces and a fixed point.
        assert_eq!(edit.new_text, format_source(source));
        assert_eq!(
            format_source(&edit.new_text),
            edit.new_text,
            "the formatted text is already canonical"
        );
    }

    #[test]
    fn an_erroring_buffer_yields_no_edits_so_source_is_not_dropped() {
        // A function header with no body is a parse error; the formatter would
        // otherwise rebuild from the partial tree and lose text.
        let source = "module a\n\npub fn f(:\n";
        let parsed = parse_source(source);
        assert!(
            parsed.has_errors(),
            "the fixture must actually fail to parse"
        );
        let index = LineIndex::new(source);
        assert!(
            formatting(source, &parsed, &index).is_none(),
            "an erroring buffer must not be reformatted"
        );
    }

    #[test]
    fn an_already_canonical_buffer_yields_no_edits() {
        let source = format_source("module a\n\nconst N: int = 1\n");
        let parsed = parse_source(&source);
        let index = LineIndex::new(source.as_str());
        assert!(
            formatting(&source, &parsed, &index).is_none(),
            "a canonical buffer needs no edit"
        );
    }
}
