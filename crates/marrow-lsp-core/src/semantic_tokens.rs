//! Map Marrow semantic token facts into LSP semantic tokens.
//!
//! The server registers a [`legend`] at initialize and answers
//! `textDocument/semanticTokens/full` by mapping transport-neutral Marrow facts
//! into the LSP's delta-encoded token stream.

mod encoding;

use std::path::Path;

use encoding::push;
use lsp_types::{SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend};
use marrow_check::{
    AnalysisSnapshot, BindingIndex,
    tooling::{
        SourceSemanticTokenModifiers, SourceSemanticTokenRole, source_semantic_token_facts,
        source_semantic_token_facts_for_file,
    },
};
use marrow_syntax::{LexedSource, ParsedSource};

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
    encode_facts(
        source_semantic_token_facts(index.text(), lexed, parsed),
        index,
    )
}

/// Like [`semantic_tokens`], but uses snapshot-bound checked facts for a fresh
/// analyzed file. Callers must only pass an index for the same source text the
/// snapshot holds for `file`.
pub fn semantic_tokens_for_file(
    snapshot: &AnalysisSnapshot,
    binding_index: &BindingIndex,
    file: &Path,
    index: &LineIndex,
) -> Option<Vec<SemanticToken>> {
    source_semantic_token_facts_for_file(snapshot, binding_index, file)
        .map(|facts| encode_facts(facts, index))
}

fn encode_facts(
    facts: Vec<marrow_check::tooling::SourceSemanticTokenFact>,
    index: &LineIndex,
) -> Vec<SemanticToken> {
    let mut tokens = Vec::new();
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;

    for fact in facts {
        push(
            &mut tokens,
            fact.span,
            token_type_for_role(fact.role),
            modifier_bits(fact.modifiers),
            index,
            &mut prev_line,
            &mut prev_start,
        );
    }

    tokens
}

fn token_type_for_role(role: SourceSemanticTokenRole) -> u32 {
    match role {
        SourceSemanticTokenRole::Keyword => TYPE_KEYWORD,
        SourceSemanticTokenRole::TypeKeyword => TYPE_TYPE,
        SourceSemanticTokenRole::StringLiteral => TYPE_STRING,
        SourceSemanticTokenRole::NumberLiteral => TYPE_NUMBER,
        SourceSemanticTokenRole::BooleanLiteral => TYPE_BOOLEAN_LITERAL,
        SourceSemanticTokenRole::Comment => TYPE_COMMENT,
        SourceSemanticTokenRole::Operator => TYPE_OPERATOR,
        SourceSemanticTokenRole::Namespace => TYPE_NAMESPACE,
        SourceSemanticTokenRole::Variable => TYPE_VARIABLE,
        SourceSemanticTokenRole::SavedRoot => TYPE_SAVED_ROOT,
        SourceSemanticTokenRole::Function => TYPE_FUNCTION,
        SourceSemanticTokenRole::Resource
        | SourceSemanticTokenRole::Surface
        | SourceSemanticTokenRole::IdentityTypeConstructor => TYPE_STRUCT,
        SourceSemanticTokenRole::Enum => TYPE_ENUM,
        SourceSemanticTokenRole::EnumMember => TYPE_ENUM_MEMBER,
        SourceSemanticTokenRole::ResourceMember | SourceSemanticTokenRole::Index => TYPE_PROPERTY,
        SourceSemanticTokenRole::Parameter | SourceSemanticTokenRole::KeyParameter => {
            TYPE_PARAMETER
        }
    }
}

fn modifier_bits(modifiers: SourceSemanticTokenModifiers) -> u32 {
    let mut bits = 0;
    if modifiers.modification {
        bits |= MOD_MODIFICATION;
    }
    if modifiers.default_library {
        bits |= MOD_DEFAULT_LIBRARY;
    }
    if modifiers.readonly {
        bits |= MOD_READONLY;
    }
    bits
}

#[cfg(test)]
mod tests;
