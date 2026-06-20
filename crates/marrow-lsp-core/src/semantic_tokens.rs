//! Classify a cached lex into LSP semantic tokens.
//!
//! The server registers a [`legend`] at initialize and answers
//! `textDocument/semanticTokens/full` by mapping the document's cached
//! [`LexedSource`] into the LSP's delta-encoded token stream. The lexer is
//! error-recovering, so this produces tokens even while the buffer has errors.

mod builtins;
mod declarations;
mod encoding;
mod references;
mod syntax;
mod type_annotations;

use std::path::Path;

use encoding::push;
use lsp_types::{SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend};
use marrow_check::{AnalysisSnapshot, BindingIndex};
use marrow_syntax::{LexedSource, ParsedSource, TokenKind};

use crate::positions::LineIndex;

/// A saved-data root (`^books`): the value type the editor's grammar cannot tell
/// from a local variable. Not in the LSP's standard set, so it is named here and
/// the legend advertises it as a custom type a theme can color.
const MARROW_SAVED_ROOT: SemanticTokenType = SemanticTokenType::new("savedRoot");

/// Boolean value literals (`true` and `false`). They are lexed as keywords but
/// colored as values so themes can distinguish them from control words.
const MARROW_BOOLEAN_LITERAL: SemanticTokenType = SemanticTokenType::new("booleanLiteral");

/// The token types this server emits, in the order their indices encode. The
/// legend the client receives is this list; a token's `token_type` field is an
/// index into it.
const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::KEYWORD,
    SemanticTokenType::TYPE,
    SemanticTokenType::STRING,
    SemanticTokenType::NUMBER,
    SemanticTokenType::COMMENT,
    SemanticTokenType::OPERATOR,
    SemanticTokenType::NAMESPACE,
    SemanticTokenType::VARIABLE,
    MARROW_SAVED_ROOT,
    SemanticTokenType::FUNCTION,
    SemanticTokenType::STRUCT,
    SemanticTokenType::ENUM,
    SemanticTokenType::ENUM_MEMBER,
    SemanticTokenType::PROPERTY,
    SemanticTokenType::PARAMETER,
    MARROW_BOOLEAN_LITERAL,
];

/// The token modifiers this server emits.
const TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[
    SemanticTokenModifier::MODIFICATION,
    SemanticTokenModifier::DEFAULT_LIBRARY,
    SemanticTokenModifier::READONLY,
];

// Indices into `TOKEN_TYPES`, kept beside it so the two cannot drift.
const TYPE_KEYWORD: u32 = 0;
const TYPE_TYPE: u32 = 1;
const TYPE_STRING: u32 = 2;
const TYPE_NUMBER: u32 = 3;
const TYPE_COMMENT: u32 = 4;
const TYPE_OPERATOR: u32 = 5;
const TYPE_NAMESPACE: u32 = 6;
const TYPE_VARIABLE: u32 = 7;
const TYPE_SAVED_ROOT: u32 = 8;
const TYPE_FUNCTION: u32 = 9;
const TYPE_STRUCT: u32 = 10;
const TYPE_ENUM: u32 = 11;
const TYPE_ENUM_MEMBER: u32 = 12;
const TYPE_PROPERTY: u32 = 13;
const TYPE_PARAMETER: u32 = 14;
const TYPE_BOOLEAN_LITERAL: u32 = 15;

/// Modifier bits, matching `TOKEN_MODIFIERS`.
const MOD_MODIFICATION: u32 = 1 << 0;
const MOD_DEFAULT_LIBRARY: u32 = 1 << 1;
const MOD_READONLY: u32 = 1 << 2;

/// The legend the server advertises in its `initialize` capabilities. Token and
/// modifier indices in [`semantic_tokens`] are positions in these lists.
pub fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: TOKEN_TYPES.to_vec(),
        token_modifiers: TOKEN_MODIFIERS.to_vec(),
    }
}

/// The classified, delta-encoded semantic tokens for a document's cached lex.
///
/// Each token is encoded relative to the previous one per the LSP wire format:
/// `delta_line`, `delta_start` within a line, `length`, the token-type index,
/// and the modifier bitset. `index` supplies the UTF-16 position mapping.
pub fn semantic_tokens(
    lexed: &LexedSource,
    parsed: &ParsedSource,
    index: &LineIndex,
) -> Vec<SemanticToken> {
    semantic_tokens_with_project_facts(lexed, parsed, index, None, None)
}

/// Like [`semantic_tokens`], with optional project facts for default-library
/// callables and resolved identifier uses. Callers supply cached facts so
/// semantic-token requests never rebuild project analysis.
pub fn semantic_tokens_with_project_facts(
    lexed: &LexedSource,
    parsed: &ParsedSource,
    index: &LineIndex,
    analysis: Option<(&AnalysisSnapshot, &Path)>,
    binding_index: Option<(&BindingIndex, &Path)>,
) -> Vec<SemanticToken> {
    let file = &parsed.file;
    let source = index.text();
    let const_declaration_overrides =
        declarations::const_declaration_overrides(lexed, file, source);
    let declaration_overrides = declarations::declaration_overrides(lexed, file, source);
    let builtin_overrides = builtins::builtin_overrides(lexed, parsed, source, analysis);
    let reference_overrides = binding_index
        .map(|(binding_index, path)| {
            references::reference_overrides(lexed, source, binding_index, path)
        })
        .unwrap_or_default();
    let type_annotation_overrides =
        type_annotations::type_annotation_overrides(lexed, file, source);

    let mut tokens = Vec::new();
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;

    let mut i = 0;
    while i < lexed.tokens.len() {
        let token = &lexed.tokens[i];

        if token.kind == TokenKind::Caret {
            push(
                &mut tokens,
                token,
                TYPE_SAVED_ROOT,
                MOD_MODIFICATION,
                index,
                &mut prev_line,
                &mut prev_start,
            );
            if let Some(name) = lexed.tokens.get(i + 1)
                && name.kind == TokenKind::Identifier
            {
                push(
                    &mut tokens,
                    name,
                    TYPE_SAVED_ROOT,
                    MOD_MODIFICATION,
                    index,
                    &mut prev_line,
                    &mut prev_start,
                );
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }

        let span = (token.span.start_byte, token.span.end_byte);
        if let Some(style) = const_declaration_overrides
            .get(&span)
            .copied()
            .or_else(|| {
                declaration_overrides
                    .get(&span)
                    .copied()
                    .map(TokenStyle::plain)
            })
            .or_else(|| builtin_overrides.get(&span).copied())
            .or_else(|| reference_overrides.get(&span).copied())
            .or_else(|| type_annotation_overrides.get(&span).copied())
            .or_else(|| syntax::token_type(token.kind).map(TokenStyle::plain))
        {
            push(
                &mut tokens,
                token,
                style.token_type,
                style.modifiers,
                index,
                &mut prev_line,
                &mut prev_start,
            );
        }
        i += 1;
    }

    tokens
}

type ByteSpan = (usize, usize);

#[derive(Debug, Clone, Copy)]
struct TokenStyle {
    token_type: u32,
    modifiers: u32,
}

impl TokenStyle {
    fn plain(token_type: u32) -> Self {
        Self {
            token_type,
            modifiers: 0,
        }
    }
}

#[cfg(test)]
mod tests;
