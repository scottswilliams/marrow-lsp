use marrow_syntax::{Keyword, LexedSource, Token, TokenKind};

pub(super) struct ActiveCall {
    pub(super) segments: Vec<String>,
    pub(super) active_parameter: usize,
    pub(super) named_argument: Option<String>,
}

pub(super) fn active_call(source: &str, lexed: &LexedSource, offset: usize) -> Option<ActiveCall> {
    let tokens = significant_tokens(lexed);
    let mut stack = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.span.start_byte >= offset {
            break;
        }
        match token.kind {
            TokenKind::LeftParen => stack.push(index),
            TokenKind::RightParen => {
                stack.pop();
            }
            _ => {}
        }
    }

    let open = *stack.last()?;
    if enclosing_parens_suppress_signature_help(source, &tokens, &stack) {
        return None;
    }
    let segments = callee_path_before(source, &tokens, open)?;
    let (active_parameter, argument_start, argument_end) = active_argument(&tokens, open, offset);
    let named_argument = named_argument(source, &tokens, argument_start, argument_end);
    Some(ActiveCall {
        segments,
        active_parameter,
        named_argument,
    })
}

fn callee_path_before(source: &str, tokens: &[Token], open: usize) -> Option<Vec<String>> {
    let mut i = open.checked_sub(1)?;
    if !is_name_segment(tokens[i].kind) {
        return None;
    }
    let mut segments = Vec::new();
    let mut root = i;
    loop {
        segments.push(tokens[i].text(source).to_string());
        let Some(double_colon) = i.checked_sub(1) else {
            break;
        };
        if tokens[double_colon].kind != TokenKind::DoubleColon {
            break;
        }
        i = double_colon.checked_sub(1)?;
        if !is_name_segment(tokens[i].kind) {
            return None;
        }
        root = i;
    }
    if !starts_at_callee_root(source, tokens, root, open) {
        return None;
    }
    segments.reverse();
    Some(segments)
}

fn starts_at_callee_root(source: &str, tokens: &[Token], index: usize, open: usize) -> bool {
    if has_declaration_prefix(source, tokens, index) {
        return false;
    }
    if looks_like_type_annotation(source, tokens, index) {
        return false;
    }
    if looks_like_resource_member_key_list(source, tokens, index, open) {
        return false;
    }
    true
}

fn has_declaration_prefix(source: &str, tokens: &[Token], index: usize) -> bool {
    let Some(previous) = index
        .checked_sub(1)
        .and_then(|previous| tokens.get(previous))
    else {
        return false;
    };
    if !same_line_between(source, previous, &tokens[index]) {
        return false;
    }
    matches!(
        previous.kind,
        TokenKind::At
            | TokenKind::DoubleColon
            | TokenKind::Dot
            | TokenKind::QuestionDot
            | TokenKind::Caret
            | TokenKind::Keyword(
                Keyword::Const
                    | Keyword::Enum
                    | Keyword::Fn
                    | Keyword::Index
                    | Keyword::Module
                    | Keyword::Required
                    | Keyword::Resource
                    | Keyword::Use
                    | Keyword::Var,
            )
    )
}

fn enclosing_parens_suppress_signature_help(
    source: &str,
    tokens: &[Token],
    stack: &[usize],
) -> bool {
    stack
        .iter()
        .take(stack.len().saturating_sub(1))
        .copied()
        .any(|open| paren_suppresses_signature_help(source, tokens, open))
}

fn paren_suppresses_signature_help(source: &str, tokens: &[Token], open: usize) -> bool {
    let Some(root) = callee_root_before(tokens, open) else {
        return false;
    };
    !starts_at_callee_root(source, tokens, root, open)
}

fn looks_like_type_annotation(source: &str, tokens: &[Token], root: usize) -> bool {
    let Some(colon_index) = root.checked_sub(1) else {
        return false;
    };
    let colon = &tokens[colon_index];
    if colon.kind != TokenKind::Colon || !same_line_between(source, colon, &tokens[root]) {
        return false;
    }
    !colon_is_named_argument_value(source, tokens, colon_index)
}

fn colon_is_named_argument_value(source: &str, tokens: &[Token], colon_index: usize) -> bool {
    let Some(name_index) = colon_index.checked_sub(1) else {
        return false;
    };
    if tokens[name_index].kind != TokenKind::Identifier
        || !same_line_between(source, &tokens[name_index], &tokens[colon_index])
    {
        return false;
    }
    let Some(open) = innermost_open_paren_before(tokens, colon_index) else {
        return false;
    };
    let Some(root) = callee_root_before(tokens, open) else {
        return false;
    };
    !has_declaration_prefix(source, tokens, root)
        && !looks_like_resource_member_key_list(source, tokens, root, open)
}

fn innermost_open_paren_before(tokens: &[Token], index: usize) -> Option<usize> {
    let mut stack = Vec::new();
    for (candidate, token) in tokens.iter().enumerate().take(index) {
        match token.kind {
            TokenKind::LeftParen => stack.push(candidate),
            TokenKind::RightParen => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack.last().copied()
}

fn callee_root_before(tokens: &[Token], open: usize) -> Option<usize> {
    let mut i = open.checked_sub(1)?;
    if !is_name_segment(tokens[i].kind) {
        return None;
    }
    let mut root = i;
    while let Some(double_colon) = i.checked_sub(1) {
        if tokens[double_colon].kind != TokenKind::DoubleColon {
            break;
        }
        i = double_colon.checked_sub(1)?;
        if !is_name_segment(tokens[i].kind) {
            return None;
        }
        root = i;
    }
    Some(root)
}

fn looks_like_resource_member_key_list(
    source: &str,
    tokens: &[Token],
    root: usize,
    open: usize,
) -> bool {
    if !is_first_significant_token_on_line(source, tokens, root) {
        return false;
    }
    if key_list_has_type_suffix(source, tokens, open) {
        return true;
    }
    is_in_resource_body(source, tokens[root].span.start_byte)
}

fn key_list_has_type_suffix(source: &str, tokens: &[Token], open: usize) -> bool {
    let Some(close) = matching_right_paren(tokens, open) else {
        return false;
    };
    let Some(next) = tokens.get(close + 1) else {
        return false;
    };
    same_line_between(source, &tokens[close], next) && next.kind == TokenKind::Colon
}

fn matching_right_paren(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open + 1) {
        match token.kind {
            TokenKind::LeftParen => depth += 1,
            TokenKind::RightParen => {
                if depth == 0 {
                    return Some(index);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

fn is_first_significant_token_on_line(source: &str, tokens: &[Token], index: usize) -> bool {
    let Some(previous) = index
        .checked_sub(1)
        .and_then(|previous| tokens.get(previous))
    else {
        return true;
    };
    !same_line_between(source, previous, &tokens[index])
}

fn same_line_between(source: &str, before: &Token, after: &Token) -> bool {
    !source[before.span.end_byte..after.span.start_byte].contains('\n')
}

fn is_in_resource_body(source: &str, byte: usize) -> bool {
    let current_line_start = line_start(source, byte);
    let current_indent = indentation(&source[current_line_start..byte]);
    if current_indent == 0 {
        return false;
    }

    for line in source[..current_line_start].lines().rev() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let indent = indentation(line);
        if indent >= current_indent {
            continue;
        }
        if starts_resource_declaration(trimmed) {
            return true;
        }
        if starts_function_declaration(trimmed) {
            return false;
        }
    }
    false
}

fn line_start(source: &str, byte: usize) -> usize {
    source[..byte].rfind('\n').map_or(0, |index| index + 1)
}

fn indentation(line: &str) -> usize {
    line.chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .count()
}

fn starts_resource_declaration(trimmed: &str) -> bool {
    trimmed.starts_with("resource ") || trimmed.starts_with("pub resource ")
}

fn starts_function_declaration(trimmed: &str) -> bool {
    trimmed.starts_with("fn ") || trimmed.starts_with("pub fn ")
}

fn active_argument(tokens: &[Token], open: usize, offset: usize) -> (usize, usize, usize) {
    let mut active = 0usize;
    let mut argument_start = open + 1;
    let mut argument_end = argument_start;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;

    for (index, token) in tokens.iter().enumerate().skip(open + 1) {
        if token.span.start_byte >= offset {
            break;
        }
        argument_end = index + 1;
        match token.kind {
            TokenKind::LeftParen => paren_depth += 1,
            TokenKind::RightParen => {
                if paren_depth == 0 {
                    argument_end = index;
                    break;
                }
                paren_depth -= 1;
            }
            TokenKind::LeftBracket => bracket_depth += 1,
            TokenKind::RightBracket => bracket_depth = bracket_depth.saturating_sub(1),
            TokenKind::Comma if paren_depth == 0 && bracket_depth == 0 => {
                active += 1;
                argument_start = index + 1;
                argument_end = argument_start;
            }
            _ => {}
        }
    }

    (active, argument_start, argument_end)
}

fn named_argument(
    source: &str,
    tokens: &[Token],
    argument_start: usize,
    argument_end: usize,
) -> Option<String> {
    let name = tokens.get(argument_start)?;
    let colon = tokens.get(argument_start + 1)?;
    (argument_start + 1 < argument_end
        && name.kind == TokenKind::Identifier
        && colon.kind == TokenKind::Colon)
        .then(|| name.text(source).to_string())
}

fn significant_tokens(lexed: &LexedSource) -> Vec<Token> {
    lexed
        .tokens
        .iter()
        .filter(|token| {
            !matches!(
                token.kind,
                TokenKind::Indent
                    | TokenKind::Dedent
                    | TokenKind::Newline
                    | TokenKind::Eof
                    | TokenKind::Comment
                    | TokenKind::DocComment
            )
        })
        .copied()
        .collect()
}

fn is_name_segment(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::Identifier | TokenKind::Keyword(_))
}
