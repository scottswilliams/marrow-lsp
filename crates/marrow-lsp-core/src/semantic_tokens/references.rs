use std::{collections::HashMap, path::Path};

use marrow_check::{BindingIndex, SymbolKind, SymbolRef};
use marrow_syntax::{LexedSource, SourceSpan, Token, TokenKind};

use super::{
    ByteSpan, MOD_READONLY, TYPE_ENUM, TYPE_ENUM_MEMBER, TYPE_FUNCTION, TYPE_NAMESPACE,
    TYPE_PARAMETER, TYPE_PROPERTY, TYPE_STRUCT, TYPE_VARIABLE, TokenStyle,
    syntax::is_path_segment_token,
};

pub(super) fn reference_overrides(
    lexed: &LexedSource,
    source: &str,
    index: &BindingIndex,
    path: &Path,
) -> HashMap<ByteSpan, TokenStyle> {
    let mut overrides = HashMap::new();
    for (token_index, token) in lexed.tokens.iter().enumerate() {
        if !is_path_segment_token(token.kind) {
            continue;
        }
        let Some(definition) = index.definition(path, token.span.start_byte) else {
            if token_is_prefix_before_resolved_namespace_tail(lexed, token_index, index, path) {
                overrides.insert(
                    (token.span.start_byte, token.span.end_byte),
                    TokenStyle::plain(TYPE_NAMESPACE),
                );
            }
            continue;
        };
        let references = index.references(&definition);
        let Some(reference) =
            best_reference_for_token(token, &references, path, source, &definition)
        else {
            if best_namespace_prefix_reference_for_token(
                token,
                &references,
                path,
                source,
                &definition,
            )
            .is_some()
                || token_is_prefix_before_resolved_namespace_tail(lexed, token_index, index, path)
            {
                overrides.insert(
                    (token.span.start_byte, token.span.end_byte),
                    TokenStyle::plain(TYPE_NAMESPACE),
                );
            }
            continue;
        };
        let Some(style) = style_for_symbol_kind(reference.kind) else {
            continue;
        };
        overrides.insert((token.span.start_byte, token.span.end_byte), style);
    }
    overrides
}

fn token_is_prefix_before_resolved_namespace_tail(
    lexed: &LexedSource,
    token_index: usize,
    index: &BindingIndex,
    path: &Path,
) -> bool {
    let mut candidate_index = token_index;
    while candidate_index + 2 < lexed.tokens.len()
        && lexed.tokens[candidate_index + 1].kind == TokenKind::DoubleColon
        && is_path_segment_token(lexed.tokens[candidate_index + 2].kind)
    {
        candidate_index += 2;
        let candidate = &lexed.tokens[candidate_index];
        if index
            .definition(path, candidate.span.start_byte)
            .is_some_and(|definition| symbol_kind_can_have_namespace_prefix(definition.kind))
        {
            return true;
        }
    }
    false
}

fn best_reference_for_token<'a>(
    token: &Token,
    references: &'a [SymbolRef],
    path: &Path,
    source: &str,
    definition: &SymbolRef,
) -> Option<&'a SymbolRef> {
    references
        .iter()
        .filter(|reference| reference.file == path)
        .filter(|reference| reference_matches_token(reference, token, source, definition))
        .min_by_key(|reference| span_width(reference.span))
}

fn best_namespace_prefix_reference_for_token<'a>(
    token: &Token,
    references: &'a [SymbolRef],
    path: &Path,
    source: &str,
    definition: &SymbolRef,
) -> Option<&'a SymbolRef> {
    references
        .iter()
        .filter(|reference| reference.file == path)
        .filter(|reference| {
            reference_matches_namespace_prefix(reference, token, source, definition)
        })
        .min_by_key(|reference| span_width(reference.span))
}

fn reference_matches_token(
    reference: &SymbolRef,
    token: &Token,
    source: &str,
    definition: &SymbolRef,
) -> bool {
    if reference.span.start_byte == token.span.start_byte
        && reference.span.end_byte == token.span.end_byte
    {
        return true;
    }
    if reference.span == definition.span {
        return false;
    }
    matches!(
        reference.kind,
        SymbolKind::Function
            | SymbolKind::Resource
            | SymbolKind::Field
            | SymbolKind::Layer
            | SymbolKind::Index
    ) && token_is_leaf_in_reference(reference.span, token, source)
}

fn reference_matches_namespace_prefix(
    reference: &SymbolRef,
    token: &Token,
    source: &str,
    definition: &SymbolRef,
) -> bool {
    if reference.span == definition.span {
        return false;
    }
    symbol_kind_can_have_namespace_prefix(reference.kind)
        && token_is_namespace_prefix_in_reference(reference.span, token, source)
}

fn token_is_leaf_in_reference(span: SourceSpan, token: &Token, source: &str) -> bool {
    if token.span.start_byte < span.start_byte || token.span.end_byte > span.end_byte {
        return false;
    }
    let Some(text) = source.get(token.span.end_byte..span.end_byte) else {
        return false;
    };
    !text
        .chars()
        .any(|ch| ch == ':' || ch == '_' || ch.is_ascii_alphanumeric())
}

fn token_is_namespace_prefix_in_reference(span: SourceSpan, token: &Token, source: &str) -> bool {
    if token.span.start_byte < span.start_byte || token.span.end_byte >= span.end_byte {
        return false;
    }
    source
        .get(token.span.end_byte..span.end_byte)
        .is_some_and(|text| text.contains("::"))
}

fn span_width(span: SourceSpan) -> usize {
    span.end_byte.saturating_sub(span.start_byte)
}

fn symbol_kind_can_have_namespace_prefix(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function | SymbolKind::Resource | SymbolKind::Enum
    )
}

fn style_for_symbol_kind(kind: SymbolKind) -> Option<TokenStyle> {
    let token_type = match kind {
        SymbolKind::Param => TYPE_PARAMETER,
        SymbolKind::Function => TYPE_FUNCTION,
        SymbolKind::Resource => TYPE_STRUCT,
        SymbolKind::Enum => TYPE_ENUM,
        SymbolKind::EnumMember => TYPE_ENUM_MEMBER,
        SymbolKind::Field | SymbolKind::Layer | SymbolKind::Index => TYPE_PROPERTY,
        SymbolKind::Local => TYPE_VARIABLE,
        SymbolKind::ModuleConst => {
            return Some(TokenStyle {
                token_type: TYPE_VARIABLE,
                modifiers: MOD_READONLY,
            });
        }
        SymbolKind::ModuleRef => TYPE_NAMESPACE,
    };
    Some(TokenStyle::plain(token_type))
}
