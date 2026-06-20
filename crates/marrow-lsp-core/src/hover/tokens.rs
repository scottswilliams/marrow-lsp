use marrow_syntax::{Keyword, Token, TokenKind, lex_source};

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

pub(super) fn qualified_name_at_with_position(
    source: &str,
    offset: usize,
) -> Option<(Vec<String>, usize)> {
    let tokens = lex_source(source).tokens;
    let (start, end, index) = qualified_name_token_bounds(&tokens, offset)?;
    let mut segments = Vec::new();
    let mut current_segment = None;
    for (token_index, token) in tokens[start..=end].iter().enumerate() {
        if token.kind == TokenKind::Identifier {
            if start + token_index == index {
                current_segment = Some(segments.len());
            }
            segments.push(token.text(source).to_string());
        }
    }
    (!segments.is_empty()).then_some((segments, current_segment?))
}

pub(super) fn module_path_at_with_position(
    source: &str,
    offset: usize,
) -> Option<(Vec<String>, usize, bool)> {
    let tokens = lex_source(source).tokens;
    let index = tokens.iter().position(|token| {
        is_path_segment_token(token.kind)
            && token.span.start_byte <= offset
            && offset <= token.span.end_byte
    })?;
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

    let mut segments = Vec::new();
    let mut cursor_segment = None;
    let mut token_index = start;
    loop {
        if !is_path_segment_token(tokens[token_index].kind) {
            return None;
        }
        if token_index == index {
            cursor_segment = Some(segments.len());
        }
        segments.push(tokens[token_index].text(source).to_string());
        if token_index == end {
            break;
        }
        if token_index + 2 > end || tokens[token_index + 1].kind != TokenKind::DoubleColon {
            return None;
        }
        token_index += 2;
    }

    let declaration = previous_significant_kind(&tokens, start)
        .is_some_and(|kind| matches!(kind, TokenKind::Keyword(Keyword::Module | Keyword::Use)));
    Some((segments, cursor_segment?, declaration))
}

pub(super) fn path_segment_index_at(tokens: &[Token], offset: usize) -> Option<usize> {
    tokens.iter().position(|token| {
        is_path_segment_token(token.kind)
            && token.span.start_byte <= offset
            && offset <= token.span.end_byte
    })
}

fn qualified_name_token_bounds(tokens: &[Token], offset: usize) -> Option<(usize, usize, usize)> {
    let index = tokens.iter().position(|token| {
        token.kind == TokenKind::Identifier
            && token.span.start_byte <= offset
            && offset <= token.span.end_byte
    })?;
    let mut start = index;
    while start >= 2
        && tokens[start - 1].kind == TokenKind::DoubleColon
        && tokens[start - 2].kind == TokenKind::Identifier
    {
        start -= 2;
    }
    let mut end = index;
    while end + 2 < tokens.len()
        && tokens[end + 1].kind == TokenKind::DoubleColon
        && tokens[end + 2].kind == TokenKind::Identifier
    {
        end += 2;
    }
    Some((start, end, index))
}

pub(super) fn std_operation_call_for_segment(
    tokens: &[Token],
    source: &str,
    index: usize,
) -> Option<(usize, usize)> {
    let candidates = [
        index
            .checked_add(2)
            .zip(index.checked_add(4))
            .map(|(module_index, op_index)| (index, module_index, op_index)),
        index
            .checked_sub(2)
            .zip(index.checked_add(2))
            .map(|(std_index, op_index)| (std_index, index, op_index)),
        index
            .checked_sub(4)
            .zip(index.checked_sub(2))
            .map(|(std_index, module_index)| (std_index, module_index, index)),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|(std_index, module_index, op_index)| {
            is_std_operation_call(tokens, source, *std_index, *module_index, *op_index)
        })
        .map(|(_, module_index, op_index)| (module_index, op_index))
}

fn is_std_operation_call(
    tokens: &[Token],
    source: &str,
    std_index: usize,
    module_index: usize,
    op_index: usize,
) -> bool {
    tokens
        .get(std_index)
        .is_some_and(|token| is_path_segment_token(token.kind) && token.text(source) == "std")
        && tokens
            .get(std_index + 1)
            .is_some_and(|token| token.kind == TokenKind::DoubleColon)
        && tokens
            .get(module_index)
            .is_some_and(|token| is_path_segment_token(token.kind))
        && tokens
            .get(module_index + 1)
            .is_some_and(|token| token.kind == TokenKind::DoubleColon)
        && tokens
            .get(op_index)
            .is_some_and(|token| is_path_segment_token(token.kind))
        && starts_at_callee_root(tokens, std_index)
        && is_call_leaf(tokens, op_index)
}

pub(super) fn bare_call_name_at<'a>(
    tokens: &[Token],
    source: &'a str,
    index: usize,
) -> Option<&'a str> {
    (is_call_leaf(tokens, index) && starts_at_callee_root(tokens, index))
        .then(|| tokens[index].text(source))
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
