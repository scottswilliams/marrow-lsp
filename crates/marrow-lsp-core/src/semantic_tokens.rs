//! Classify a cached lex into LSP semantic tokens.
//!
//! The server registers a [`legend`] at initialize and answers
//! `textDocument/semanticTokens/full` by mapping the document's cached
//! [`LexedSource`] — never a re-lex — into the LSP's delta-encoded token stream.
//! The lexer is error-recovering, so this produces tokens even while the buffer
//! has errors.
//!
//! The baseline pass colors lexer categories, then a small parsed overlay gives
//! declaration names their editor roles. The saved-root sigil `^` and the root
//! name that follows it remain a dedicated token type (`MARROW_SAVED_ROOT`) with
//! the `MODIFICATION` modifier, so a durable `^books` reads differently from a
//! local variable named `books`.

use std::collections::HashMap;
use std::path::Path;

use lsp_types::{SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend};
use marrow_check::{BindingIndex, SymbolKind, SymbolRef};
use marrow_schema::stdlib;
use marrow_syntax::{
    Block, Declaration, EnumMember, EvolveStep, FieldDecl, GroupDecl, IndexDecl, Keyword,
    LexedSource, ResourceMember, SourceFile, SourceSpan, Statement, Token, TokenKind, TypeRef,
};

use crate::{
    language_facts::{self, BareBuiltinKind},
    positions::LineIndex,
};

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
/// `delta_line`, `delta_start` (within a line, in UTF-16 units), `length`, the
/// token-type index, and the modifier bitset. `index` supplies the UTF-16
/// position mapping. Trivia the editor does not color (indentation, newlines,
/// the synthetic EOF) is skipped; everything else maps to one token.
pub fn semantic_tokens(
    lexed: &LexedSource,
    file: &SourceFile,
    index: &LineIndex,
) -> Vec<SemanticToken> {
    semantic_tokens_with_bindings(lexed, file, index, None)
}

/// Like [`semantic_tokens`], with optional project binding facts for resolved
/// identifier uses. The binding index is supplied by the caller so semantic-token
/// requests never rebuild project analysis.
pub fn semantic_tokens_with_bindings(
    lexed: &LexedSource,
    file: &SourceFile,
    index: &LineIndex,
    binding_index: Option<(&BindingIndex, &Path)>,
) -> Vec<SemanticToken> {
    let mut tokens = Vec::new();
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;
    let const_declaration_overrides = const_declaration_overrides(lexed, file, index.text());
    let declaration_overrides = declaration_overrides(lexed, file, index.text());
    let reference_overrides = binding_index
        .map(|(binding_index, path)| reference_overrides(lexed, index.text(), binding_index, path))
        .unwrap_or_default();
    let type_annotation_overrides = type_annotation_overrides(lexed, file, index.text());
    let builtin_overrides = builtin_overrides(lexed, index.text());

    let mut i = 0;
    while i < lexed.tokens.len() {
        let token = &lexed.tokens[i];

        // A `^` sigil and the identifier it precedes are one logical saved root.
        // Emitting both with the saved-root type — and consuming the name here —
        // is what distinguishes `^books` from a local `books`.
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

        if let Some(style) = const_declaration_overrides
            .get(&(token.span.start_byte, token.span.end_byte))
            .copied()
            .or_else(|| {
                declaration_overrides
                    .get(&(token.span.start_byte, token.span.end_byte))
                    .copied()
                    .map(TokenStyle::plain)
            })
            .or_else(|| {
                builtin_overrides
                    .get(&(token.span.start_byte, token.span.end_byte))
                    .copied()
            })
            .or_else(|| {
                reference_overrides
                    .get(&(token.span.start_byte, token.span.end_byte))
                    .copied()
            })
            .or_else(|| {
                type_annotation_overrides
                    .get(&(token.span.start_byte, token.span.end_byte))
                    .copied()
            })
            .or_else(|| token_type(token.kind).map(TokenStyle::plain))
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

fn const_declaration_overrides(
    lexed: &LexedSource,
    file: &SourceFile,
    source: &str,
) -> HashMap<ByteSpan, TokenStyle> {
    let mut overrides = HashMap::new();
    for declaration in &file.declarations {
        if let Declaration::Const(const_decl) = declaration {
            add_first_identifier_style_override(
                &mut overrides,
                lexed,
                source,
                const_decl.span,
                &const_decl.name,
                TokenStyle {
                    token_type: TYPE_VARIABLE,
                    modifiers: MOD_READONLY,
                },
            );
        }
    }
    overrides
}

fn declaration_overrides(
    lexed: &LexedSource,
    file: &SourceFile,
    source: &str,
) -> HashMap<ByteSpan, u32> {
    let mut overrides = HashMap::new();

    if let Some(module) = &file.module {
        add_qualified_path_after_keyword(
            &mut overrides,
            lexed,
            source,
            module.span,
            Keyword::Module,
            &module.name,
            TYPE_NAMESPACE,
        );
    }
    for use_decl in &file.uses {
        add_qualified_path_after_keyword(
            &mut overrides,
            lexed,
            source,
            use_decl.span,
            Keyword::Use,
            &use_decl.name,
            TYPE_NAMESPACE,
        );
    }

    for declaration in &file.declarations {
        match declaration {
            Declaration::Const(_) => {}
            Declaration::Function(function) => {
                if let Some(name_index) = add_first_identifier_override(
                    &mut overrides,
                    lexed,
                    source,
                    function.span,
                    &function.name,
                    TYPE_FUNCTION,
                ) && !function.params.is_empty()
                    && let Some((open, close)) = matching_parens_after(
                        lexed,
                        function.span,
                        lexed.tokens[name_index].span.end_byte,
                    )
                {
                    add_colon_name_overrides(
                        &mut overrides,
                        lexed,
                        source,
                        open,
                        close,
                        function.params.iter().map(|param| param.name.as_str()),
                        TYPE_PARAMETER,
                    );
                }
            }
            Declaration::Resource(resource) => {
                add_first_identifier_override(
                    &mut overrides,
                    lexed,
                    source,
                    resource.span,
                    &resource.name,
                    TYPE_STRUCT,
                );
                for member in &resource.members {
                    add_resource_member_overrides(&mut overrides, lexed, source, member);
                }
            }
            Declaration::Store(store) => {
                if !store.root.keys.is_empty() {
                    add_saved_root_key_overrides(
                        &mut overrides,
                        lexed,
                        source,
                        store.span,
                        &store.root.root,
                        store.root.keys.iter().map(|key| key.name.as_str()),
                    );
                }
                for index in &store.indexes {
                    add_index_overrides(&mut overrides, lexed, source, index);
                }
            }
            Declaration::Enum(enum_decl) => {
                add_first_identifier_override(
                    &mut overrides,
                    lexed,
                    source,
                    enum_decl.span,
                    &enum_decl.name,
                    TYPE_ENUM,
                );
                for member in &enum_decl.members {
                    add_enum_member_overrides(&mut overrides, lexed, source, member);
                }
            }
            Declaration::Evolve(evolve) => {
                for step in &evolve.steps {
                    add_evolve_step_overrides(&mut overrides, lexed, source, step);
                }
            }
        }
    }

    overrides
}

fn add_evolve_step_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    step: &EvolveStep,
) {
    let name = match step {
        EvolveStep::Rename { .. } => "rename",
        EvolveStep::Default { .. } => "default",
        EvolveStep::Retire { .. } => "retire",
        EvolveStep::Transform { .. } => "transform",
    };
    add_first_identifier_override(overrides, lexed, source, step.span(), name, TYPE_KEYWORD);
}

fn add_resource_member_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    member: &ResourceMember,
) {
    match member {
        ResourceMember::Field(field) => add_field_overrides(overrides, lexed, source, field),
        ResourceMember::Group(group) => add_group_overrides(overrides, lexed, source, group),
    }
}

fn add_field_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    field: &FieldDecl,
) {
    if let Some(name_index) = add_first_identifier_override(
        overrides,
        lexed,
        source,
        field.span,
        &field.name,
        TYPE_PROPERTY,
    ) && !field.keys.is_empty()
        && let Some((open, close)) =
            matching_parens_after(lexed, field.span, lexed.tokens[name_index].span.end_byte)
    {
        add_colon_name_overrides(
            overrides,
            lexed,
            source,
            open,
            close,
            field.keys.iter().map(|key| key.name.as_str()),
            TYPE_PARAMETER,
        );
    }
}

fn add_group_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    group: &GroupDecl,
) {
    if let Some(name_index) = add_first_identifier_override(
        overrides,
        lexed,
        source,
        group.span,
        &group.name,
        TYPE_PROPERTY,
    ) && !group.keys.is_empty()
        && let Some((open, close)) =
            matching_parens_after(lexed, group.span, lexed.tokens[name_index].span.end_byte)
    {
        add_colon_name_overrides(
            overrides,
            lexed,
            source,
            open,
            close,
            group.keys.iter().map(|key| key.name.as_str()),
            TYPE_PARAMETER,
        );
    }
    for member in &group.members {
        add_resource_member_overrides(overrides, lexed, source, member);
    }
}

fn add_index_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    index: &IndexDecl,
) {
    if let Some(name_index) = add_first_identifier_override(
        overrides,
        lexed,
        source,
        index.span,
        &index.name,
        TYPE_PROPERTY,
    ) && !index.args.is_empty()
        && let Some((open, close)) =
            matching_parens_after(lexed, index.span, lexed.tokens[name_index].span.end_byte)
    {
        add_argument_name_overrides(
            overrides,
            lexed,
            source,
            open,
            close,
            index.args.iter().map(|arg| arg.as_str()),
            TYPE_PARAMETER,
        );
    }
}

fn add_enum_member_overrides(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    member: &EnumMember,
) {
    add_first_identifier_override(
        overrides,
        lexed,
        source,
        member.span,
        &member.name,
        TYPE_ENUM_MEMBER,
    );
    for child in &member.members {
        add_enum_member_overrides(overrides, lexed, source, child);
    }
}

fn add_qualified_path_after_keyword(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    span: SourceSpan,
    keyword: Keyword,
    path: &str,
    token_type: u32,
) {
    let Some(keyword_token) = lexed
        .tokens
        .iter()
        .find(|token| token_in_span(token, span) && token.kind == TokenKind::Keyword(keyword))
    else {
        return;
    };
    let mut cursor = keyword_token.span.end_byte;
    for segment in path.split("::") {
        let Some(token) = lexed.tokens.iter().find(|token| {
            token.span.start_byte >= cursor
                && token_in_span(token, span)
                && is_path_segment_token(token.kind)
                && token.text(source) == segment
        }) else {
            return;
        };
        insert_override(overrides, token, token_type);
        cursor = token.span.end_byte;
    }
}

fn add_saved_root_key_overrides<'a>(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    span: SourceSpan,
    root: &str,
    names: impl IntoIterator<Item = &'a str>,
) {
    let Some(caret_index) = lexed
        .tokens
        .iter()
        .position(|token| token_in_span(token, span) && token.kind == TokenKind::Caret)
    else {
        return;
    };
    let Some(root_token) = lexed.tokens[caret_index + 1..].iter().find(|token| {
        token_in_span(token, span)
            && token.kind == TokenKind::Identifier
            && token.text(source) == root
    }) else {
        return;
    };
    let Some((open, close)) = matching_parens_after(lexed, span, root_token.span.end_byte) else {
        return;
    };
    add_colon_name_overrides(overrides, lexed, source, open, close, names, TYPE_PARAMETER);
}

fn add_first_identifier_override(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    span: SourceSpan,
    name: &str,
    token_type: u32,
) -> Option<usize> {
    let index = first_identifier_index(lexed, source, span, name)?;
    insert_override(overrides, &lexed.tokens[index], token_type);
    Some(index)
}

fn add_first_identifier_style_override(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    span: SourceSpan,
    name: &str,
    style: TokenStyle,
) -> Option<usize> {
    let index = first_identifier_index(lexed, source, span, name)?;
    insert_style_override(overrides, &lexed.tokens[index], style);
    Some(index)
}

fn first_identifier_index(
    lexed: &LexedSource,
    source: &str,
    span: SourceSpan,
    name: &str,
) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    lexed.tokens.iter().position(|token| {
        token_in_span(token, span)
            && token.kind == TokenKind::Identifier
            && token.text(source) == name
    })
}

fn add_colon_name_overrides<'a>(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    open: usize,
    close: usize,
    names: impl IntoIterator<Item = &'a str>,
    token_type: u32,
) {
    let names: Vec<&str> = names.into_iter().collect();
    for index in open + 1..close {
        let token = &lexed.tokens[index];
        if token.kind == TokenKind::Identifier
            && names.iter().any(|name| *name == token.text(source))
            && lexed
                .tokens
                .get(index + 1)
                .is_some_and(|next| next.kind == TokenKind::Colon)
        {
            insert_override(overrides, token, token_type);
        }
    }
}

fn add_argument_name_overrides<'a>(
    overrides: &mut HashMap<ByteSpan, u32>,
    lexed: &LexedSource,
    source: &str,
    open: usize,
    close: usize,
    names: impl IntoIterator<Item = &'a str>,
    token_type: u32,
) {
    let names: Vec<&str> = names.into_iter().collect();
    for index in open + 1..close {
        let token = &lexed.tokens[index];
        if token.kind == TokenKind::Identifier
            && names.iter().any(|name| *name == token.text(source))
        {
            insert_override(overrides, token, token_type);
        }
    }
}

fn matching_parens_after(
    lexed: &LexedSource,
    span: SourceSpan,
    after_byte: usize,
) -> Option<(usize, usize)> {
    let open = lexed.tokens.iter().position(|token| {
        token.span.start_byte >= after_byte
            && token_in_span(token, span)
            && token.kind == TokenKind::LeftParen
    })?;
    let mut depth = 0usize;
    for index in open..lexed.tokens.len() {
        let token = &lexed.tokens[index];
        if !token_in_span(token, span) {
            break;
        }
        match token.kind {
            TokenKind::LeftParen => depth += 1,
            TokenKind::RightParen => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((open, index));
                }
            }
            _ => {}
        }
    }
    None
}

fn insert_override(overrides: &mut HashMap<ByteSpan, u32>, token: &Token, token_type: u32) {
    overrides.insert((token.span.start_byte, token.span.end_byte), token_type);
}

fn insert_style_override(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    token: &Token,
    style: TokenStyle,
) {
    overrides.insert((token.span.start_byte, token.span.end_byte), style);
}

fn token_in_span(token: &Token, span: SourceSpan) -> bool {
    token.span.start_byte >= span.start_byte && token.span.end_byte <= span.end_byte
}

fn is_path_segment_token(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::Identifier | TokenKind::Keyword(_))
}

fn reference_overrides(
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
            .is_some_and(|definition| resolved_tail_allows_namespace_prefix(definition.kind))
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

fn resolved_tail_allows_namespace_prefix(kind: SymbolKind) -> bool {
    matches!(kind, SymbolKind::Resource | SymbolKind::Enum)
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
        SymbolKind::ModuleRef => return None,
    };
    Some(TokenStyle::plain(token_type))
}

fn type_annotation_overrides(
    lexed: &LexedSource,
    file: &SourceFile,
    source: &str,
) -> HashMap<ByteSpan, TokenStyle> {
    let mut overrides = HashMap::new();
    for declaration in &file.declarations {
        match declaration {
            Declaration::Const(const_decl) => {
                if let Some(ty) = &const_decl.ty {
                    add_type_annotation_overrides(&mut overrides, lexed, source, ty);
                }
            }
            Declaration::Function(function) => {
                for param in &function.params {
                    add_type_annotation_overrides(&mut overrides, lexed, source, &param.ty);
                }
                if let Some(ty) = &function.return_type {
                    add_type_annotation_overrides(&mut overrides, lexed, source, ty);
                }
                add_block_type_annotation_overrides(&mut overrides, lexed, source, &function.body);
            }
            Declaration::Resource(resource) => {
                for member in &resource.members {
                    add_resource_member_type_annotation_overrides(
                        &mut overrides,
                        lexed,
                        source,
                        member,
                    );
                }
            }
            Declaration::Store(store) => {
                for key in &store.root.keys {
                    add_type_annotation_overrides(&mut overrides, lexed, source, &key.ty);
                }
            }
            Declaration::Evolve(evolve) => {
                for step in &evolve.steps {
                    add_evolve_step_type_annotation_overrides(&mut overrides, lexed, source, step);
                }
            }
            Declaration::Enum(_) => {}
        }
    }
    overrides
}

fn add_evolve_step_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    step: &EvolveStep,
) {
    match step {
        EvolveStep::Transform { body, .. } => {
            add_block_type_annotation_overrides(overrides, lexed, source, body);
        }
        EvolveStep::Rename { .. } | EvolveStep::Default { .. } | EvolveStep::Retire { .. } => {}
    }
}

fn add_block_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    block: &Block,
) {
    for statement in &block.statements {
        match statement {
            Statement::Const { ty, .. } => {
                if let Some(ty) = ty {
                    add_type_annotation_overrides(overrides, lexed, source, ty);
                }
            }
            Statement::Var { keys, ty, .. } => {
                for key in keys {
                    add_type_annotation_overrides(overrides, lexed, source, &key.ty);
                }
                if let Some(ty) = ty {
                    add_type_annotation_overrides(overrides, lexed, source, ty);
                }
            }
            Statement::If {
                then_block,
                else_ifs,
                else_block,
                ..
            } => {
                add_block_type_annotation_overrides(overrides, lexed, source, then_block);
                for else_if in else_ifs {
                    add_block_type_annotation_overrides(overrides, lexed, source, &else_if.block);
                }
                if let Some(else_block) = else_block {
                    add_block_type_annotation_overrides(overrides, lexed, source, else_block);
                }
            }
            Statement::While { body, .. }
            | Statement::For { body, .. }
            | Statement::Transaction { body, .. } => {
                add_block_type_annotation_overrides(overrides, lexed, source, body);
            }
            Statement::Try {
                body,
                catch,
                finally,
                ..
            } => {
                add_block_type_annotation_overrides(overrides, lexed, source, body);
                if let Some(catch) = catch {
                    if let Some(ty) = &catch.ty {
                        add_type_annotation_overrides(overrides, lexed, source, ty);
                    }
                    add_block_type_annotation_overrides(overrides, lexed, source, &catch.block);
                }
                if let Some(finally) = finally {
                    add_block_type_annotation_overrides(overrides, lexed, source, finally);
                }
            }
            Statement::Match { arms, .. } => {
                for arm in arms {
                    add_block_type_annotation_overrides(overrides, lexed, source, &arm.block);
                }
            }
            Statement::Assign { .. }
            | Statement::Delete { .. }
            | Statement::Return { .. }
            | Statement::Break { .. }
            | Statement::Continue { .. }
            | Statement::Throw { .. }
            | Statement::Expr { .. } => {}
        }
    }
}

fn add_resource_member_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    member: &ResourceMember,
) {
    match member {
        ResourceMember::Field(field) => {
            for key in &field.keys {
                add_type_annotation_overrides(overrides, lexed, source, &key.ty);
            }
            add_type_annotation_overrides(overrides, lexed, source, &field.ty);
        }
        ResourceMember::Group(group) => {
            for key in &group.keys {
                add_type_annotation_overrides(overrides, lexed, source, &key.ty);
            }
            for member in &group.members {
                add_resource_member_type_annotation_overrides(overrides, lexed, source, member);
            }
        }
    }
}

fn add_type_annotation_overrides(
    overrides: &mut HashMap<ByteSpan, TokenStyle>,
    lexed: &LexedSource,
    source: &str,
    ty: &TypeRef,
) {
    let significant_tokens = lexed
        .tokens
        .iter()
        .filter(|token| token_in_span(token, ty.span) && !is_trivia(token.kind))
        .collect::<Vec<_>>();

    for (index, tokens) in significant_tokens.windows(5).enumerate() {
        let [id_token, open, caret, root_token, close] = tokens else {
            continue;
        };
        if !is_path_segment_token(id_token.kind)
            || id_token.text(source) != "Id"
            || open.kind != TokenKind::LeftParen
            || caret.kind != TokenKind::Caret
            || !is_path_segment_token(root_token.kind)
            || close.kind != TokenKind::RightParen
            || index
                .checked_sub(1)
                .and_then(|previous| significant_tokens.get(previous))
                .is_some_and(|token| token.kind == TokenKind::DoubleColon)
            || significant_tokens
                .get(index + 5)
                .is_some_and(|token| token.kind == TokenKind::DoubleColon)
        {
            continue;
        }

        overrides.insert(
            (id_token.span.start_byte, id_token.span.end_byte),
            TokenStyle::plain(TYPE_STRUCT),
        );
    }
}

fn builtin_overrides(lexed: &LexedSource, source: &str) -> HashMap<ByteSpan, TokenStyle> {
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

/// The semantic token type for a lexer token kind, or `None` for trivia and
/// structural tokens the editor does not color (indentation, newlines, EOF,
/// parentheses, commas).
fn token_type(kind: TokenKind) -> Option<u32> {
    match kind {
        TokenKind::Identifier => Some(TYPE_VARIABLE),
        TokenKind::Integer | TokenKind::Decimal | TokenKind::Duration => Some(TYPE_NUMBER),
        // The whole string — including a `$"...{x}..."` interpolation's literal
        // segments and braces — colors as a string; the interpolated expression
        // tokens between them are lexed as their own kinds and color normally.
        TokenKind::String
        | TokenKind::Bytes
        | TokenKind::InterpolationStart
        | TokenKind::InterpolationText
        | TokenKind::InterpolationEnd => Some(TYPE_STRING),
        TokenKind::Comment | TokenKind::DocComment => Some(TYPE_COMMENT),
        TokenKind::Keyword(keyword) => Some(match keyword {
            Keyword::True | Keyword::False => TYPE_BOOLEAN_LITERAL,
            _ if is_operator_keyword(keyword) => TYPE_OPERATOR,
            _ if is_type_keyword(keyword) => TYPE_TYPE,
            _ => TYPE_KEYWORD,
        }),
        TokenKind::DoubleColon => Some(TYPE_NAMESPACE),
        TokenKind::Colon
        | TokenKind::Dot
        | TokenKind::DotDot
        | TokenKind::DotDotEqual
        | TokenKind::Equal
        | TokenKind::EqualEqual
        | TokenKind::BangEqual
        | TokenKind::QuestionDot
        | TokenKind::QuestionQuestion
        | TokenKind::Less
        | TokenKind::LessEqual
        | TokenKind::Greater
        | TokenKind::GreaterEqual
        | TokenKind::Plus
        | TokenKind::Minus
        | TokenKind::Star
        | TokenKind::Slash
        | TokenKind::Percent
        | TokenKind::Underscore
        | TokenKind::At => Some(TYPE_OPERATOR),
        // The `^` sigil is handled before this is reached; structural delimiters
        // and the interpolation-expression braces are left to the grammar.
        _ => None,
    }
}

fn is_operator_keyword(keyword: Keyword) -> bool {
    matches!(
        keyword,
        Keyword::Not | Keyword::And | Keyword::Or | Keyword::Is
    )
}

/// Whether a keyword names a type (so it colors as a type, not a control word).
/// These are exactly the built-in type keywords a type annotation may use.
fn is_type_keyword(keyword: Keyword) -> bool {
    matches!(
        keyword,
        Keyword::Int
            | Keyword::Decimal
            | Keyword::Bool
            | Keyword::String
            | Keyword::Bytes
            | Keyword::Date
            | Keyword::Instant
            | Keyword::Duration
            | Keyword::Sequence
            | Keyword::Unknown
            | Keyword::Error
            | Keyword::ErrorCode
    )
}

/// Delta-encode one token against the running cursor and push it. `prev_line` and
/// `prev_start` track the previous emitted token's position so each token carries
/// only its delta, as the LSP wire format requires.
#[allow(clippy::too_many_arguments)]
fn push(
    out: &mut Vec<SemanticToken>,
    token: &Token,
    token_type: u32,
    token_modifiers_bitset: u32,
    index: &LineIndex,
    prev_line: &mut u32,
    prev_start: &mut u32,
) {
    let start = index.position(token.span.start_byte);
    let end = index.position(token.span.end_byte);
    // A token spanning lines (only a multi-line string can) is clamped to the
    // remainder of its first line; the lexer keeps such tokens on one line in
    // practice, so this is a guard, not a common path.
    let length = if end.line == start.line {
        end.character.saturating_sub(start.character)
    } else {
        first_line_length(token, index, start.character)
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

fn first_line_length(token: &Token, index: &LineIndex, start_character: u32) -> u32 {
    let text = index.text();
    let token_text = &text[token.span.start_byte..token.span.end_byte.min(text.len())];
    let line_end = token_text
        .find('\n')
        .map(|offset| token.span.start_byte + offset)
        .unwrap_or(token.span.end_byte);
    let end = index.position(line_end);
    end.character.saturating_sub(start_character).max(1)
}

#[cfg(test)]
mod tests;
