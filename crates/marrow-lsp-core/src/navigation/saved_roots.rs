use std::path::Path;

use marrow_syntax::{TokenKind, lex_source};

pub(super) fn root_syntax_at(
    snapshot: &marrow_check::AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> bool {
    let Some(analyzed) = snapshot.files.iter().find(|analyzed| analyzed.path == file) else {
        return false;
    };
    let tokens = lex_source(&analyzed.source).tokens;
    tokens.windows(2).any(|pair| {
        let [previous, token] = pair else {
            return false;
        };
        previous.kind == TokenKind::Caret
            && token.kind == TokenKind::Identifier
            && previous.span.start_byte <= offset
            && offset <= token.span.end_byte
    })
}
