use std::collections::HashMap;

use marrow_schema::stdlib;
use marrow_syntax::{LexedSource, Token, TokenKind};

use crate::language_facts::{self, BareBuiltinKind};

use super::{
    ByteSpan, MOD_DEFAULT_LIBRARY, TYPE_FUNCTION, TYPE_NAMESPACE, TYPE_TYPE, TokenStyle,
    syntax::{is_path_segment_token, is_trivia},
};

pub(super) fn builtin_overrides(
    lexed: &LexedSource,
    source: &str,
) -> HashMap<ByteSpan, TokenStyle> {
    let mut overrides = HashMap::new();
    for (index, token) in lexed.tokens.iter().enumerate() {
        if !is_call_leaf(lexed, index) {
            continue;
        }
        let text = token.text(source);
        let token_type = if is_std_library_call(lexed, source, index) {
            Some(TYPE_FUNCTION)
        } else if is_bare_call_leaf(lexed, index) {
            match language_facts::bare_builtin_kind(text) {
                Some(BareBuiltinKind::Function) => Some(TYPE_FUNCTION),
                Some(BareBuiltinKind::ScalarConversion) => Some(TYPE_TYPE),
                None => None,
            }
        } else {
            None
        };
        if let Some(token_type) = token_type {
            if is_std_library_call(lexed, source, index) {
                insert_builtin_namespace_prefix(&mut overrides, &lexed.tokens[index - 4]);
                insert_builtin_namespace_prefix(&mut overrides, &lexed.tokens[index - 2]);
            }
            overrides.insert(
                (token.span.start_byte, token.span.end_byte),
                TokenStyle {
                    token_type,
                    modifiers: MOD_DEFAULT_LIBRARY,
                },
            );
        }
    }
    overrides
}

fn insert_builtin_namespace_prefix(overrides: &mut HashMap<ByteSpan, TokenStyle>, token: &Token) {
    overrides.insert(
        (token.span.start_byte, token.span.end_byte),
        TokenStyle {
            token_type: TYPE_NAMESPACE,
            modifiers: MOD_DEFAULT_LIBRARY,
        },
    );
}

fn is_call_leaf(lexed: &LexedSource, index: usize) -> bool {
    is_path_segment_token(lexed.tokens[index].kind)
        && lexed
            .tokens
            .get(index + 1)
            .is_some_and(|token| token.kind == TokenKind::LeftParen)
}

fn is_bare_call_leaf(lexed: &LexedSource, index: usize) -> bool {
    is_call_leaf(lexed, index) && starts_at_callee_root(lexed, index)
}

fn is_std_library_call(lexed: &LexedSource, source: &str, index: usize) -> bool {
    index >= 4
        && lexed.tokens[index - 1].kind == TokenKind::DoubleColon
        && is_path_segment_token(lexed.tokens[index - 2].kind)
        && lexed.tokens[index - 3].kind == TokenKind::DoubleColon
        && is_path_segment_token(lexed.tokens[index - 4].kind)
        && lexed.tokens[index - 4].text(source) == "std"
        && starts_at_callee_root(lexed, index - 4)
        && stdlib::lookup(
            lexed.tokens[index - 2].text(source),
            lexed.tokens[index].text(source),
        )
        .is_some()
}

fn starts_at_callee_root(lexed: &LexedSource, index: usize) -> bool {
    !previous_significant_kind(lexed, index).is_some_and(|kind| {
        matches!(
            kind,
            TokenKind::DoubleColon | TokenKind::Dot | TokenKind::QuestionDot
        )
    })
}

fn previous_significant_kind(lexed: &LexedSource, index: usize) -> Option<TokenKind> {
    lexed.tokens[..index]
        .iter()
        .rev()
        .find(|token| !is_trivia(token.kind))
        .map(|token| token.kind)
}
