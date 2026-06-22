use lsp_types::SemanticToken;
use marrow_syntax::SourceSpan;

use crate::positions::LineIndex;

/// Delta-encode one token against the running cursor and push it. `prev_line` and
/// `prev_start` track the previous emitted token's position so each token carries
/// only its delta, as the LSP wire format requires.
#[allow(clippy::too_many_arguments)]
pub(super) fn push(
    out: &mut Vec<SemanticToken>,
    span: SourceSpan,
    token_type: u32,
    token_modifiers_bitset: u32,
    index: &LineIndex,
    prev_line: &mut u32,
    prev_start: &mut u32,
) {
    let start = index.position(span.start_byte);
    let end = index.position(span.end_byte);
    let length = if end.line == start.line {
        end.character.saturating_sub(start.character)
    } else {
        first_line_length(span, index, start.character)
    };

    let delta_line = start.line - *prev_line;
    let delta_start = if delta_line == 0 {
        start.character - *prev_start
    } else {
        start.character
    };
    out.push(SemanticToken {
        delta_line,
        delta_start,
        length,
        token_type,
        token_modifiers_bitset,
    });
    *prev_line = start.line;
    *prev_start = start.character;
}

fn first_line_length(span: SourceSpan, index: &LineIndex, start_character: u32) -> u32 {
    let text = index.text();
    let token_text = &text[span.start_byte..span.end_byte.min(text.len())];
    let line_end = token_text
        .find('\n')
        .map(|offset| span.start_byte + offset)
        .unwrap_or(span.end_byte);
    let end = index.position(line_end);
    end.character.saturating_sub(start_character).max(1)
}
