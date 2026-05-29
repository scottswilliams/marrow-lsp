//! Mapping between UTF-8 byte offsets and LSP positions.
//!
//! Marrow spans carry UTF-8 byte offsets. LSP positions carry a line and a
//! `character` that counts **UTF-16 code units** — the LSP wire unit. Conversion
//! is isolated here so the UTF-16 handling lives in exactly one place. (The old
//! in-tree `marrow lsp` counted Unicode scalar values, which is correct only
//! within the Basic Multilingual Plane; an astral character such as an emoji
//! occupies two UTF-16 units, so its columns were off.)

/// An LSP position: a zero-based line and a zero-based `character` measured in
/// UTF-16 code units from the start of that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// A per-document index from byte offsets to line/character positions and back.
/// Built once per document edit.
pub struct LineIndex {
    /// Byte offset of the first character of each line. Always starts with `0`.
    line_starts: Vec<usize>,
    text: String,
}

impl LineIndex {
    /// Build the index for a document.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let mut line_starts = vec![0];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(offset + 1);
            }
        }
        Self { line_starts, text }
    }

    /// Convert a UTF-8 byte offset to an LSP position. A byte past the end of the
    /// document, or one that does not fall on a character boundary, clamps to the
    /// nearest boundary at or before it.
    pub fn position(&self, byte: usize) -> Position {
        let byte = self.floor_char_boundary(byte.min(self.text.len()));
        let line = match self.line_starts.binary_search(&byte) {
            Ok(line) => line,
            Err(next) => next - 1,
        };
        let line_start = self.line_starts[line];
        let character = self.text[line_start..byte]
            .chars()
            .map(char::len_utf16)
            .sum::<usize>() as u32;
        Position {
            line: line as u32,
            character,
        }
    }

    /// Convert an LSP position to a UTF-8 byte offset. A `character` past the end
    /// of its line clamps to the line end (before the newline); a `line` past the
    /// end of the document clamps to the document end.
    pub fn offset(&self, position: Position) -> usize {
        let line = position.line as usize;
        let Some(&line_start) = self.line_starts.get(line) else {
            return self.text.len();
        };
        let line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text.len());

        let mut utf16 = 0u32;
        let mut byte = line_start;
        for ch in self.text[line_start..line_end].chars() {
            if utf16 >= position.character || ch == '\n' {
                break;
            }
            utf16 += ch.len_utf16() as u32;
            byte += ch.len_utf8();
        }
        byte
    }

    /// The document text this index was built over.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The LSP range a UTF-8 byte span covers, with both ends mapped through
    /// [`Self::position`]. Marrow spans carry byte offsets; clients want UTF-16
    /// ranges, and routing every span through here keeps that mapping in one place.
    pub fn range(&self, start_byte: usize, end_byte: usize) -> lsp_types::Range {
        lsp_types::Range {
            start: to_lsp_position(self.position(start_byte)),
            end: to_lsp_position(self.position(end_byte)),
        }
    }

    /// The largest character boundary at or before `byte` (which must be within
    /// `0..=text.len()`).
    fn floor_char_boundary(&self, mut byte: usize) -> usize {
        while byte > 0 && !self.text.is_char_boundary(byte) {
            byte -= 1;
        }
        byte
    }
}

/// Map this crate's [`Position`] to the LSP wire position. The two are the same
/// shape; the wrapper type keeps the UTF-16 contract documented on [`Position`].
fn to_lsp_position(position: Position) -> lsp_types::Position {
    lsp_types::Position {
        line: position.line,
        character: position.character,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn ascii_offsets_map_to_line_and_character() {
        let index = LineIndex::new("abc\ndef\n");
        assert_eq!(index.position(0), pos(0, 0));
        assert_eq!(index.position(2), pos(0, 2));
        assert_eq!(index.position(3), pos(0, 3)); // the newline ends line 0
        assert_eq!(index.position(4), pos(1, 0)); // start of line 1
        assert_eq!(index.position(6), pos(1, 2));
    }

    #[test]
    fn astral_character_counts_two_utf16_units() {
        // U+1F600 is four UTF-8 bytes but two UTF-16 code units (a surrogate pair).
        let text = "a\u{1F600}b";
        let index = LineIndex::new(text);
        assert_eq!(index.position(1), pos(0, 1)); // before the emoji
        // 'b' is character 3, not 2: the emoji is two UTF-16 units.
        assert_eq!(index.position(1 + "\u{1F600}".len()), pos(0, 3));
    }

    #[test]
    fn bmp_non_ascii_counts_one_utf16_unit() {
        // U+00E9 (é) is two UTF-8 bytes but one UTF-16 code unit.
        let text = "\u{00E9}=";
        let index = LineIndex::new(text);
        assert_eq!(index.position("\u{00E9}".len()), pos(0, 1));
    }

    #[test]
    fn offset_is_the_inverse_of_position_at_every_boundary() {
        let text = "let x = 1\nlet \u{03B8} = \u{1F600}\nend";
        let index = LineIndex::new(text);
        let boundaries = text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain(std::iter::once(text.len()));
        for byte in boundaries {
            let position = index.position(byte);
            assert_eq!(index.offset(position), byte, "round-trip at byte {byte}");
        }
    }

    #[test]
    fn byte_past_end_clamps_to_document_end() {
        let index = LineIndex::new("ab");
        assert_eq!(index.position(999), pos(0, 2));
    }

    #[test]
    fn character_past_line_end_clamps_to_line_end() {
        let index = LineIndex::new("ab\ncd");
        assert_eq!(index.offset(pos(0, 99)), 2); // before the newline
        assert_eq!(index.offset(pos(99, 0)), 5); // line past the end
    }

    #[test]
    fn position_floors_to_a_character_boundary() {
        let text = "\u{1F600}"; // a single four-byte character
        let index = LineIndex::new(text);
        // A byte in the middle of the character floors to its start.
        assert_eq!(index.position(2), pos(0, 0));
    }
}
