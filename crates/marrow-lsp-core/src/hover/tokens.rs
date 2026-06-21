use marrow_syntax::{Keyword, Token, TokenKind};

pub(super) fn significant_neighbors(
    tokens: &[Token],
    index: usize,
) -> (Option<TokenKind>, Option<TokenKind>) {
    (
        previous_significant_kind(tokens, index),
        next_significant_kind(tokens, index),
    )
}

fn next_significant_kind(tokens: &[Token], index: usize) -> Option<TokenKind> {
    tokens
        .get(index + 1..)?
        .iter()
        .find(|token| !is_trivia(token.kind))
        .map(|token| token.kind)
}

pub(super) fn path_segment_index_at(tokens: &[Token], offset: usize) -> Option<usize> {
    tokens.iter().position(|token| {
        is_path_segment_token(token.kind)
            && token.span.start_byte <= offset
            && offset <= token.span.end_byte
    })
}

pub(super) fn bare_call_name_at<'a>(
    tokens: &[Token],
    source: &'a str,
    index: usize,
) -> Option<&'a str> {
    (is_call_leaf(tokens, index) && starts_at_callee_root(tokens, index))
        .then(|| tokens[index].text(source))
}

pub(super) fn callable_path_at(
    tokens: &[Token],
    source: &str,
    index: usize,
) -> Option<(Vec<String>, usize)> {
    if let Some(name) = bare_call_name_at(tokens, source, index) {
        return Some((vec![name.to_string()], index));
    }

    let mut start = index;
    while start >= 2
        && tokens[start - 1].kind == TokenKind::DoubleColon
        && is_path_segment_token(tokens[start - 2].kind)
    {
        start -= 2;
    }
    let mut end = index;
    while end + 2 < tokens.len()
        && tokens[end + 1].kind == TokenKind::DoubleColon
        && is_path_segment_token(tokens[end + 2].kind)
    {
        end += 2;
    }
    if !starts_at_callee_root(tokens, start) || !is_call_leaf(tokens, end) {
        return None;
    }

    let mut segments = Vec::new();
    let mut cursor = start;
    loop {
        if !is_path_segment_token(tokens[cursor].kind) {
            return None;
        }
        segments.push(tokens[cursor].text(source).to_string());
        if cursor == end {
            break;
        }
        if cursor + 2 > end || tokens[cursor + 1].kind != TokenKind::DoubleColon {
            return None;
        }
        cursor += 2;
    }
    Some((segments, end))
}

fn is_call_leaf(tokens: &[Token], index: usize) -> bool {
    is_path_segment_token(tokens[index].kind)
        && tokens
            .get(index + 1)
            .is_some_and(|token| token.kind == TokenKind::LeftParen)
}

fn starts_at_callee_root(tokens: &[Token], index: usize) -> bool {
    !previous_significant_kind(tokens, index).is_some_and(|kind| {
        matches!(
            kind,
            TokenKind::DoubleColon
                | TokenKind::Dot
                | TokenKind::QuestionDot
                | TokenKind::Keyword(Keyword::Fn)
        )
    })
}

fn previous_significant_kind(tokens: &[Token], index: usize) -> Option<TokenKind> {
    tokens[..index]
        .iter()
        .rev()
        .find(|token| !is_trivia(token.kind))
        .map(|token| token.kind)
}

fn is_path_segment_token(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::Identifier | TokenKind::Keyword(_))
}

fn is_trivia(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Indent
            | TokenKind::Dedent
            | TokenKind::Newline
            | TokenKind::Eof
            | TokenKind::Comment
            | TokenKind::DocComment
    )
}
