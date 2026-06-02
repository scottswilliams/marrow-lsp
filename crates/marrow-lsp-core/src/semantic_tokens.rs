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
    Block, Declaration, EnumMember, FieldDecl, GroupDecl, IndexDecl, Keyword, LexedSource,
    ResourceMember, SourceFile, SourceSpan, Statement, Token, TokenKind, TypeRef,
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

        if let Some(style) = declaration_overrides
            .get(&(token.span.start_byte, token.span.end_byte))
            .copied()
            .map(TokenStyle::plain)
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
        }
    }

    overrides
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
    if name.is_empty() {
        return None;
    }
    let index = lexed.tokens.iter().position(|token| {
        token_in_span(token, span)
            && token.kind == TokenKind::Identifier
            && token.text(source) == name
    })?;
    insert_override(overrides, &lexed.tokens[index], token_type);
    Some(index)
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
        SymbolKind::ModuleConst | SymbolKind::ModuleRef => return None,
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
            Declaration::Enum(_) => {}
        }
    }
    overrides
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
            | Statement::Transaction { body, .. }
            | Statement::Lock { body, .. } => {
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
            | Statement::Merge { .. }
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
mod tests {
    use super::*;
    use marrow_check::{ProjectSources, analyze_project, build_binding_index};
    use marrow_project::parse_config;
    use marrow_syntax::{lex_source, parse_source};

    type DecodedToken = (u32, u32, u32, u32, u32);
    type DecodedTokens = Vec<DecodedToken>;

    /// Decode the delta-encoded tokens back to absolute `(line, character, length,
    /// type, modifiers)` tuples so a test can assert on positions directly.
    fn absolute(tokens: &[SemanticToken]) -> DecodedTokens {
        let mut line = 0;
        let mut start = 0;
        tokens
            .iter()
            .map(|token| {
                if token.delta_line == 0 {
                    start += token.delta_start;
                } else {
                    line += token.delta_line;
                    start = token.delta_start;
                }
                (
                    line,
                    start,
                    token.length,
                    token.token_type,
                    token.token_modifiers_bitset,
                )
            })
            .collect()
    }

    fn legend_index(semantic_type: &SemanticTokenType) -> u32 {
        legend()
            .token_types
            .iter()
            .position(|candidate| candidate == semantic_type)
            .unwrap_or_else(|| panic!("legend should contain {semantic_type:?}")) as u32
    }

    fn modifier_bit(modifier: &SemanticTokenModifier) -> u32 {
        let index = legend()
            .token_modifiers
            .iter()
            .position(|candidate| candidate == modifier)
            .unwrap_or_else(|| panic!("legend should contain {modifier:?}"));
        1 << index
    }

    fn decoded_for(source: &str) -> (LineIndex, DecodedTokens) {
        let index = LineIndex::new(source);
        let parsed = parse_source(source);
        let decoded = absolute(&semantic_tokens(&lex_source(source), &parsed.file, &index));
        (index, decoded)
    }

    fn decoded_for_checked(source: &str) -> (LineIndex, DecodedTokens) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("m.mw");
        std::fs::write(&file, source).unwrap();

        let config = parse_config(r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let snapshot = analyze_project(root, &config, &ProjectSources::new()).unwrap();
        let binding_index = build_binding_index(&snapshot);
        let parsed = snapshot
            .files
            .iter()
            .find(|analyzed| analyzed.path == file)
            .expect("analyzed source file");
        let index = LineIndex::new(source);
        let decoded = absolute(&semantic_tokens_with_bindings(
            &lex_source(source),
            &parsed.parsed.file,
            &index,
            Some((&binding_index, &file)),
        ));
        assert!(
            !snapshot.program.modules.is_empty(),
            "checked fixture should provide binding facts"
        );
        (index, decoded)
    }

    fn decoded_for_checked_file(
        files: &[(&str, &str)],
        active_relative: &str,
    ) -> (LineIndex, DecodedTokens) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        for (relative, source) in files {
            let file = src.join(relative);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(file, source).unwrap();
        }

        let active_source = files
            .iter()
            .find_map(|(relative, source)| (*relative == active_relative).then_some(*source))
            .expect("active source file should be in the fixture");
        let active_file = src.join(active_relative);
        let config = parse_config(r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let snapshot = analyze_project(root, &config, &ProjectSources::new()).unwrap();
        let binding_index = build_binding_index(&snapshot);
        let parsed = snapshot
            .files
            .iter()
            .find(|analyzed| analyzed.path == active_file)
            .expect("analyzed active source file");
        let index = LineIndex::new(active_source);
        let decoded = absolute(&semantic_tokens_with_bindings(
            &lex_source(active_source),
            &parsed.parsed.file,
            &index,
            Some((&binding_index, &active_file)),
        ));
        assert!(
            !snapshot.program.modules.is_empty(),
            "checked fixture should provide binding facts"
        );
        (index, decoded)
    }

    fn assert_token_type(
        source: &str,
        index: &LineIndex,
        decoded: &[DecodedToken],
        line_text: &str,
        lexeme: &str,
        expected_type: u32,
    ) {
        let line_start = source
            .find(line_text)
            .unwrap_or_else(|| panic!("source should contain line {line_text:?}"));
        let in_line = line_text
            .find(lexeme)
            .unwrap_or_else(|| panic!("line {line_text:?} should contain {lexeme:?}"));
        let position = index.position(line_start + in_line);
        let length = lexeme.encode_utf16().count() as u32;
        let token = decoded
            .iter()
            .find(|(line, start, len, _, _)| {
                *line == position.line && *start == position.character && *len == length
            })
            .unwrap_or_else(|| {
                panic!(
                    "semantic token for {lexeme:?} at {}:{} should exist in {decoded:?}",
                    position.line, position.character
                )
            });
        assert_eq!(
            token.3, expected_type,
            "{lexeme:?} on {line_text:?} should have semantic token type {expected_type}"
        );
    }

    fn assert_token(
        source: &str,
        index: &LineIndex,
        decoded: &[DecodedToken],
        line_text: &str,
        lexeme: &str,
        expected_type: u32,
        expected_modifiers: u32,
    ) {
        let line_start = source
            .find(line_text)
            .unwrap_or_else(|| panic!("source should contain line {line_text:?}"));
        let in_line = line_text
            .find(lexeme)
            .unwrap_or_else(|| panic!("line {line_text:?} should contain {lexeme:?}"));
        let position = index.position(line_start + in_line);
        let length = lexeme.encode_utf16().count() as u32;
        let token = decoded
            .iter()
            .find(|(line, start, len, _, _)| {
                *line == position.line && *start == position.character && *len == length
            })
            .unwrap_or_else(|| {
                panic!(
                    "semantic token for {lexeme:?} at {}:{} should exist in {decoded:?}",
                    position.line, position.character
                )
            });
        assert_eq!(
            (token.3, token.4),
            (expected_type, expected_modifiers),
            "{lexeme:?} on {line_text:?} should have semantic token type/modifiers"
        );
    }

    #[test]
    fn legend_lists_standard_lsp_declaration_types() {
        let legend = legend();
        for semantic_type in [
            SemanticTokenType::FUNCTION,
            SemanticTokenType::STRUCT,
            SemanticTokenType::ENUM,
            SemanticTokenType::ENUM_MEMBER,
            SemanticTokenType::PROPERTY,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::NAMESPACE,
        ] {
            assert!(
                legend.token_types.contains(&semantic_type),
                "legend should contain {semantic_type:?}"
            );
        }
    }

    #[test]
    fn the_legend_lists_the_default_library_modifier() {
        assert!(
            legend()
                .token_modifiers
                .contains(&SemanticTokenModifier::DEFAULT_LIBRARY),
            "the legend must advertise the defaultLibrary modifier"
        );
    }

    #[test]
    fn boolean_literal_token_is_appended_after_parameter() {
        let boolean_literal = SemanticTokenType::new("booleanLiteral");

        assert_eq!(
            legend_index(&boolean_literal),
            legend_index(&SemanticTokenType::PARAMETER) + 1,
            "booleanLiteral should be appended after parameter so existing token indices stay stable"
        );
    }

    #[test]
    fn true_and_false_color_as_boolean_literals() {
        let source = "\
module m

fn f(): bool
    return true and false
";
        let (index, decoded) = decoded_for(source);
        let boolean_literal = SemanticTokenType::new("booleanLiteral");
        let boolean_literal = legend_index(&boolean_literal);

        assert_token(
            source,
            &index,
            &decoded,
            "    return true and false",
            "true",
            boolean_literal,
            0,
        );
        assert_token(
            source,
            &index,
            &decoded,
            "    return true and false",
            "false",
            boolean_literal,
            0,
        );
    }

    #[test]
    fn concat_underscore_colors_as_operator() {
        let source = "\
module m

fn f(a: string, b: string): string
    return a _ b
";
        let (index, decoded) = decoded_for(source);

        assert_token(
            source,
            &index,
            &decoded,
            "    return a _ b",
            "_",
            legend_index(&SemanticTokenType::OPERATOR),
            0,
        );
    }

    #[test]
    fn std_namespaces_are_default_library() {
        let source = "\
module m

fn f()
    const len = std::text::length(\"abc\")
";
        let (index, decoded) = decoded_for(source);
        let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);

        assert_token(
            source,
            &index,
            &decoded,
            "    const len = std::text::length(\"abc\")",
            "std",
            legend_index(&SemanticTokenType::NAMESPACE),
            default_library,
        );
        assert_token(
            source,
            &index,
            &decoded,
            "    const len = std::text::length(\"abc\")",
            "text",
            legend_index(&SemanticTokenType::NAMESPACE),
            default_library,
        );
        assert_token(
            source,
            &index,
            &decoded,
            "    const len = std::text::length(\"abc\")",
            "length",
            legend_index(&SemanticTokenType::FUNCTION),
            default_library,
        );
    }

    #[test]
    fn declaration_identifiers_use_role_specific_token_types() {
        let source = "\
module shelf::catalog
use shared::imports

resource Book at ^books(id: int)
    required title: string
    notes(note_id: string)
        text: string
    tags(pos: int): string
    index byTitle(title, id)

pub fn paint(book_id: int, label: string): int
    return book_id

pub enum Genre
    Fiction
        Literary
    Nonfiction
";
        let (index, decoded) = decoded_for(source);

        let namespace = legend_index(&SemanticTokenType::NAMESPACE);
        assert_token_type(
            source,
            &index,
            &decoded,
            "module shelf::catalog",
            "shelf",
            namespace,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "module shelf::catalog",
            "catalog",
            namespace,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "use shared::imports",
            "shared",
            namespace,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "use shared::imports",
            "imports",
            namespace,
        );

        assert_token_type(
            source,
            &index,
            &decoded,
            "resource Book at ^books(id: int)",
            "Book",
            legend_index(&SemanticTokenType::STRUCT),
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "pub fn paint(book_id: int, label: string): int",
            "paint",
            legend_index(&SemanticTokenType::FUNCTION),
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "pub enum Genre",
            "Genre",
            legend_index(&SemanticTokenType::ENUM),
        );

        let enum_member = legend_index(&SemanticTokenType::ENUM_MEMBER);
        assert_token_type(
            source,
            &index,
            &decoded,
            "    Fiction",
            "Fiction",
            enum_member,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "        Literary",
            "Literary",
            enum_member,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    Nonfiction",
            "Nonfiction",
            enum_member,
        );

        let property = legend_index(&SemanticTokenType::PROPERTY);
        assert_token_type(
            source,
            &index,
            &decoded,
            "    required title: string",
            "title",
            property,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    notes(note_id: string)",
            "notes",
            property,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "        text: string",
            "text",
            property,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    tags(pos: int): string",
            "tags",
            property,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    index byTitle(title, id)",
            "byTitle",
            property,
        );

        let parameter = legend_index(&SemanticTokenType::PARAMETER);
        assert_token_type(
            source,
            &index,
            &decoded,
            "resource Book at ^books(id: int)",
            "id",
            parameter,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    notes(note_id: string)",
            "note_id",
            parameter,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    tags(pos: int): string",
            "pos",
            parameter,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    index byTitle(title, id)",
            "title",
            parameter,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    index byTitle(title, id)",
            "id",
            parameter,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "pub fn paint(book_id: int, label: string): int",
            "book_id",
            parameter,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "pub fn paint(book_id: int, label: string): int",
            "label",
            parameter,
        );
    }

    #[test]
    fn saved_root_sigil_and_name_are_marked_distinctly_from_a_variable() {
        // `^books` reads/writes saved data; `books` is just a local. They must not
        // carry the same token type.
        let source = "module a\n\npub fn f()\n    const books: int = 1\n    delete ^books(books)\n";
        let index = LineIndex::new(source);
        let parsed = parse_source(source);
        let tokens = semantic_tokens(&lex_source(source), &parsed.file, &index);
        let decoded = absolute(&tokens);

        // The saved-root tokens carry the dedicated type and the modification bit.
        let saved: Vec<_> = decoded
            .iter()
            .filter(|(_, _, _, ty, _)| *ty == TYPE_SAVED_ROOT)
            .collect();
        assert!(
            !saved.is_empty(),
            "the `^books` saved root should produce saved-root tokens"
        );
        assert!(
            saved.iter().all(|(_, _, _, _, m)| *m == MOD_MODIFICATION),
            "saved-root tokens carry the modification modifier"
        );

        // A plain variable token never uses the saved-root type.
        assert!(
            decoded
                .iter()
                .any(|(_, _, _, ty, m)| *ty == TYPE_VARIABLE && *m == 0),
            "the local `books` should be a plain variable token"
        );
    }

    #[test]
    fn keywords_types_numbers_strings_and_comments_are_classified() {
        let source = "module a\n\nconst N: int = 42 ; a count\n";
        let index = LineIndex::new(source);
        let parsed = parse_source(source);
        let decoded = absolute(&semantic_tokens(&lex_source(source), &parsed.file, &index));
        let types: std::collections::HashSet<u32> =
            decoded.iter().map(|(_, _, _, ty, _)| *ty).collect();
        assert!(
            types.contains(&TYPE_KEYWORD),
            "`module`/`const` are keywords"
        );
        assert!(types.contains(&TYPE_TYPE), "`int` is a type keyword");
        assert!(types.contains(&TYPE_NUMBER), "`42` is a number");
        assert!(
            types.contains(&TYPE_COMMENT),
            "the `;` comment is a comment"
        );
    }

    #[test]
    fn namespace_separator_is_its_own_type() {
        let source = "module a\n\npub fn f()\n    std::clock::now()\n";
        let index = LineIndex::new(source);
        let parsed = parse_source(source);
        let decoded = absolute(&semantic_tokens(&lex_source(source), &parsed.file, &index));
        assert!(
            decoded.iter().any(|(_, _, _, ty, _)| *ty == TYPE_NAMESPACE),
            "the `::` separator colors as a namespace operator"
        );
    }

    #[test]
    fn resolved_reference_uses_take_their_symbol_roles() {
        let source = "\
module m

resource Book at ^books(id: int)
    required title: string
    tags(pos: int): string
    index byTitle(title)

enum Status
    active
    archived

fn helper(id: int): int
    return id

fn paint(id: int, title: string): int
    const book = Book(id)
    const status = Status::archived
    const found = ^books(id).title
    const tag = ^books(id).tags(1)
    const lookup = ^books.byTitle(title)
    return helper(id)
";
        let (index, decoded) = decoded_for_checked(source);

        assert_token_type(
            source,
            &index,
            &decoded,
            "    return helper(id)",
            "id",
            legend_index(&SemanticTokenType::PARAMETER),
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    return helper(id)",
            "helper",
            legend_index(&SemanticTokenType::FUNCTION),
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    const book = Book(id)",
            "Book",
            legend_index(&SemanticTokenType::STRUCT),
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    const status = Status::archived",
            "archived",
            legend_index(&SemanticTokenType::ENUM_MEMBER),
        );

        let property = legend_index(&SemanticTokenType::PROPERTY);
        assert_token_type(
            source,
            &index,
            &decoded,
            "    const found = ^books(id).title",
            "title",
            property,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    const tag = ^books(id).tags(1)",
            "tags",
            property,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    const lookup = ^books.byTitle(title)",
            "byTitle",
            property,
        );
    }

    #[test]
    fn declaration_header_type_tokens_are_not_recolored_by_definition_spans() {
        let source = "\
module m

resource Book at ^books(id: int)
    required title: string
    tags(pos: int): string

fn helper(id: int): int
    return id
";
        let (index, decoded) = decoded_for_checked(source);
        let ty = legend_index(&SemanticTokenType::TYPE);

        assert_token_type(
            source,
            &index,
            &decoded,
            "resource Book at ^books(id: int)",
            "int",
            ty,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    required title: string",
            "string",
            ty,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "    tags(pos: int): string",
            "string",
            ty,
        );
        assert_token_type(
            source,
            &index,
            &decoded,
            "fn helper(id: int): int",
            "int",
            ty,
        );
    }

    #[test]
    fn enum_type_annotation_references_color_as_enums() {
        let source = "\
module m

enum Status
    active
    archived

fn f()
    const current: Status = Status::active
";
        let (index, decoded) = decoded_for_checked(source);

        assert_token_type(
            source,
            &index,
            &decoded,
            "    const current: Status = Status::active",
            "Status",
            legend_index(&SemanticTokenType::ENUM),
        );
    }

    #[test]
    fn checked_qualified_enum_type_prefix_is_namespace_while_leaf_stays_enum() {
        let status_source = "\
module a::b

pub enum Status
    active
    archived
";
        let app_source = "\
module app

use a::b

fn f(): b::Status
    return b::Status::active
";
        let (index, decoded) = decoded_for_checked_file(
            &[("a/b.mw", status_source), ("app.mw", app_source)],
            "app.mw",
        );

        assert_token(
            app_source,
            &index,
            &decoded,
            "fn f(): b::Status",
            "b",
            legend_index(&SemanticTokenType::NAMESPACE),
            0,
        );
        assert_token(
            app_source,
            &index,
            &decoded,
            "fn f(): b::Status",
            "Status",
            legend_index(&SemanticTokenType::ENUM),
            0,
        );
    }

    #[test]
    fn checked_qualified_enum_member_prefixes_are_namespaces_while_leaves_keep_roles() {
        let status_source = "\
module a::b

pub enum Status
    active
    archived
";
        let app_source = "\
module app

use a::b

fn f(): b::Status
    return b::Status::active
";
        let (index, decoded) = decoded_for_checked_file(
            &[("a/b.mw", status_source), ("app.mw", app_source)],
            "app.mw",
        );

        assert_token(
            app_source,
            &index,
            &decoded,
            "    return b::Status::active",
            "b",
            legend_index(&SemanticTokenType::NAMESPACE),
            0,
        );
        assert_token(
            app_source,
            &index,
            &decoded,
            "    return b::Status::active",
            "Status",
            legend_index(&SemanticTokenType::ENUM),
            0,
        );
        assert_token(
            app_source,
            &index,
            &decoded,
            "    return b::Status::active",
            "active",
            legend_index(&SemanticTokenType::ENUM_MEMBER),
            0,
        );
    }

    #[test]
    fn checked_fully_qualified_enum_type_prefixes_are_namespaces_while_leaf_stays_enum() {
        let status_source = "\
module a::b::c

pub enum Status
    active
    archived
";
        let app_source = "\
module app

fn f(): a::b::c::Status
    return a::b::c::Status::active
";
        let (index, decoded) = decoded_for_checked_file(
            &[("a/b/c.mw", status_source), ("app.mw", app_source)],
            "app.mw",
        );
        let namespace = legend_index(&SemanticTokenType::NAMESPACE);

        for lexeme in ["a", "b", "c"] {
            assert_token(
                app_source,
                &index,
                &decoded,
                "fn f(): a::b::c::Status",
                lexeme,
                namespace,
                0,
            );
        }
        assert_token(
            app_source,
            &index,
            &decoded,
            "fn f(): a::b::c::Status",
            "Status",
            legend_index(&SemanticTokenType::ENUM),
            0,
        );
    }

    #[test]
    fn known_default_library_calls_carry_default_library_modifier() {
        let source = "\
module m

resource Book at ^books(id: int)
    tags(pos: int): string

fn f(): bool
    const clock_value = std::clock::now()
    const has_book = exists(^books(1))
    const ids = keys(^books)
    const added = append(^books(1).tags, \"tag\")
    const next = nextId(^books)
    const c1 = int(\"1\")
    const c2 = decimal(\"1.5\")
    const c3 = string(1)
    const c4 = bool(\"true\")
    return has_book
";
        let (index, decoded) = decoded_for_checked(source);
        let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);
        let function = legend_index(&SemanticTokenType::FUNCTION);
        let ty = legend_index(&SemanticTokenType::TYPE);

        for (line, lexeme, expected_type) in [
            ("    const clock_value = std::clock::now()", "now", function),
            ("    const has_book = exists(^books(1))", "exists", function),
            ("    const ids = keys(^books)", "keys", function),
            (
                "    const added = append(^books(1).tags, \"tag\")",
                "append",
                function,
            ),
            ("    const next = nextId(^books)", "nextId", function),
            ("    const c1 = int(\"1\")", "int", ty),
            ("    const c2 = decimal(\"1.5\")", "decimal", ty),
            ("    const c3 = string(1)", "string", ty),
            ("    const c4 = bool(\"true\")", "bool", ty),
        ] {
            assert_token(
                source,
                &index,
                &decoded,
                line,
                lexeme,
                expected_type,
                default_library,
            );
        }
    }

    #[test]
    fn standard_library_calls_use_the_shared_std_table() {
        let source = "\
module m

fn f()
    const text_len = std::text::length(\"abc\")
    std::assert::isTrue(true)
    const unknown_value = std::text::missing(\"abc\")
";
        let (index, decoded) = decoded_for_checked(source);
        let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);
        let function = legend_index(&SemanticTokenType::FUNCTION);
        let namespace = legend_index(&SemanticTokenType::NAMESPACE);

        assert_token(
            source,
            &index,
            &decoded,
            "    std::assert::isTrue(true)",
            "std",
            namespace,
            default_library,
        );
        assert_token(
            source,
            &index,
            &decoded,
            "    std::assert::isTrue(true)",
            "assert",
            namespace,
            default_library,
        );
        assert_token(
            source,
            &index,
            &decoded,
            "    const text_len = std::text::length(\"abc\")",
            "length",
            function,
            default_library,
        );
        assert_token(
            source,
            &index,
            &decoded,
            "    std::assert::isTrue(true)",
            "isTrue",
            function,
            default_library,
        );
        assert_token(
            source,
            &index,
            &decoded,
            "    const unknown_value = std::text::missing(\"abc\")",
            "missing",
            legend_index(&SemanticTokenType::VARIABLE),
            0,
        );
    }

    #[test]
    fn checked_qualified_call_prefixes_are_namespaces_while_leaf_keeps_role() {
        let source = "\
module m

pub fn helper(): int
    return 1

fn f(): int
    return m::helper()
";
        let (index, decoded) = decoded_for_checked(source);

        assert_token(
            source,
            &index,
            &decoded,
            "    return m::helper()",
            "m",
            legend_index(&SemanticTokenType::NAMESPACE),
            0,
        );
        assert_token(
            source,
            &index,
            &decoded,
            "    return m::helper()",
            "helper",
            legend_index(&SemanticTokenType::FUNCTION),
            0,
        );
    }

    #[test]
    fn checked_qualified_resource_constructor_prefix_is_namespace_while_leaf_stays_struct() {
        let state_source = "\
module shelf::state

resource Book at ^state_books(id: int)
    required title: string
";
        let app_source = "\
module shelf::app

use shelf::state

resource Book at ^app_books(code: string)
    required label: string

pub fn make()
    const book = state::Book(title: \"x\")
";
        let (index, decoded) = decoded_for_checked_file(
            &[
                ("shelf/state.mw", state_source),
                ("shelf/app.mw", app_source),
            ],
            "shelf/app.mw",
        );

        assert_token(
            app_source,
            &index,
            &decoded,
            "    const book = state::Book(title: \"x\")",
            "state",
            legend_index(&SemanticTokenType::NAMESPACE),
            0,
        );
        assert_token(
            app_source,
            &index,
            &decoded,
            "    const book = state::Book(title: \"x\")",
            "Book",
            legend_index(&SemanticTokenType::STRUCT),
            0,
        );
    }

    #[test]
    fn checked_canonical_identity_type_annotation_colors_id_as_struct() {
        let source = "\
module m

resource Author
    name: string

store ^authors(id: int): Author

pub fn f(
    id: Id(^authors),
): Id(^authors)
    return id
";
        let (index, decoded) = decoded_for_checked(source);

        assert_token(
            source,
            &index,
            &decoded,
            "    id: Id(^authors),",
            "Id",
            legend_index(&SemanticTokenType::STRUCT),
            0,
        );
        assert_token(
            source,
            &index,
            &decoded,
            "): Id(^authors)",
            "Id",
            legend_index(&SemanticTokenType::STRUCT),
            0,
        );
    }

    #[test]
    fn checked_nested_canonical_identity_type_annotations_color_id_as_struct() {
        let source = "\
module m

resource Author
    name: string

store ^authors(id: int): Author

pub fn f(
    ids: sequence[Id(^authors)],
): sequence[Id(^authors)]
    return ids
";
        let (index, decoded) = decoded_for_checked(source);
        let strukt = legend_index(&SemanticTokenType::STRUCT);

        assert_token(
            source,
            &index,
            &decoded,
            "    ids: sequence[Id(^authors)],",
            "Id",
            strukt,
            0,
        );
        assert_token(
            source,
            &index,
            &decoded,
            "): sequence[Id(^authors)]",
            "Id",
            strukt,
            0,
        );
    }

    #[test]
    fn local_canonical_identity_type_annotations_color_id_as_struct() {
        let source = "\
module m

resource Author
    name: string

store ^authors(id: int): Author

fn f(): Id(^authors)
    const direct: Id(^authors) = nextId(^authors)
    var later: Id(^authors) = direct
    return direct
";
        let (index, decoded) = decoded_for_checked(source);
        let strukt = legend_index(&SemanticTokenType::STRUCT);

        for line in [
            "    const direct: Id(^authors) = nextId(^authors)",
            "    var later: Id(^authors) = direct",
        ] {
            assert_token(source, &index, &decoded, line, "Id", strukt, 0);
        }
    }

    #[test]
    fn longer_canonical_identity_type_paths_are_not_colored_as_store_identity() {
        let source = "\
module m

resource Author
    name: string

store ^authors(id: int): Author

fn bad_extra(x: Id(^authors)::Extra)
    return
";
        let (index, decoded) = decoded_for(source);
        let variable = legend_index(&SemanticTokenType::VARIABLE);

        assert_token(
            source,
            &index,
            &decoded,
            "fn bad_extra(x: Id(^authors)::Extra)",
            "Id",
            variable,
            0,
        );
    }

    #[test]
    fn std_library_call_must_start_at_callee_root() {
        let source = "\
module m

fn f()
    const text_len = foo::std::text::length(\"abc\")
";
        let (index, decoded) = decoded_for(source);

        assert_token(
            source,
            &index,
            &decoded,
            "    const text_len = foo::std::text::length(\"abc\")",
            "length",
            legend_index(&SemanticTokenType::VARIABLE),
            0,
        );
    }

    #[test]
    fn bare_builtin_call_must_not_be_qualified_leaf() {
        let source = "\
module m

fn f()
    const found = foo::exists()
";
        let (index, decoded) = decoded_for(source);

        assert_token(
            source,
            &index,
            &decoded,
            "    const found = foo::exists()",
            "exists",
            legend_index(&SemanticTokenType::VARIABLE),
            0,
        );
    }

    #[test]
    fn checker_single_name_builtins_are_default_library_calls() {
        let source = "\
module m

resource Book at ^books(id: int)
    required title: string

fn f()
    const items = keys(^books)
    const rev_items = reversed(items)
    const after = next(items)
    const before = prev(items)
";
        let (index, decoded) = decoded_for(source);
        let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);
        let function = legend_index(&SemanticTokenType::FUNCTION);

        for (line, lexeme) in [
            ("    const rev_items = reversed(items)", "reversed"),
            ("    const after = next(items)", "next"),
            ("    const before = prev(items)", "prev"),
        ] {
            assert_token(
                source,
                &index,
                &decoded,
                line,
                lexeme,
                function,
                default_library,
            );
        }
    }

    #[test]
    fn builtin_call_leaf_wins_over_user_function_binding_reference() {
        let source = "\
module m

resource Book at ^books(id: int)
    required title: string

fn exists(value: unknown): bool
    return false

fn f(): bool
    return exists(^books(1))
";
        let (index, decoded) = decoded_for_checked(source);
        let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);

        assert_token(
            source,
            &index,
            &decoded,
            "    return exists(^books(1))",
            "exists",
            legend_index(&SemanticTokenType::FUNCTION),
            default_library,
        );
        assert_token(
            source,
            &index,
            &decoded,
            "fn exists(value: unknown): bool",
            "exists",
            legend_index(&SemanticTokenType::FUNCTION),
            0,
        );
    }

    #[test]
    fn scalar_conversion_calls_use_the_shared_scalar_names() {
        let source = "\
module m

fn f()
    const b1 = bytes(\"abc\")
    const d1 = date(\"2026-05-30\")
    const i1 = instant(\"2026-05-30T00:00:00+00:00\")
    const span1 = duration(\"1.hour\")
";
        let (index, decoded) = decoded_for_checked(source);
        let default_library = modifier_bit(&SemanticTokenModifier::DEFAULT_LIBRARY);
        let ty = legend_index(&SemanticTokenType::TYPE);

        for (line, lexeme) in [
            ("    const b1 = bytes(\"abc\")", "bytes"),
            ("    const d1 = date(\"2026-05-30\")", "date"),
            (
                "    const i1 = instant(\"2026-05-30T00:00:00+00:00\")",
                "instant",
            ),
            ("    const span1 = duration(\"1.hour\")", "duration"),
        ] {
            assert_token(source, &index, &decoded, line, lexeme, ty, default_library);
        }
    }

    #[test]
    fn word_operators_color_as_operators() {
        let source = "\
module m

fn f(a: bool, b: bool, value: unknown): bool
    return not a or a and b or value is string
";
        let (index, decoded) = decoded_for(source);
        let operator = legend_index(&SemanticTokenType::OPERATOR);

        for lexeme in ["not", "or", "and", "is"] {
            assert_token_type(
                source,
                &index,
                &decoded,
                "    return not a or a and b or value is string",
                lexeme,
                operator,
            );
        }
    }

    #[test]
    fn duration_literals_color_as_numbers() {
        let source = "module m\n\nconst WAIT: duration = 1.hour\n";
        let (index, decoded) = decoded_for(source);
        assert_token_type(
            source,
            &index,
            &decoded,
            "const WAIT: duration = 1.hour",
            "1.hour",
            legend_index(&SemanticTokenType::NUMBER),
        );
    }

    #[test]
    fn multi_line_tokens_are_clamped_to_the_first_line_remainder() {
        let source = "let value = \"first\nsecond\"";
        let token = Token {
            kind: marrow_syntax::TokenKind::String,
            span: marrow_syntax::SourceSpan {
                start_byte: source.find('"').unwrap(),
                end_byte: source.len(),
                line: 0,
                column: 12,
            },
        };
        let index = LineIndex::new(source);
        let mut tokens = Vec::new();
        let mut line = 0;
        let mut start = 0;

        push(
            &mut tokens,
            &token,
            TYPE_STRING,
            0,
            &index,
            &mut line,
            &mut start,
        );

        let decoded = absolute(&tokens);
        assert_eq!(decoded[0].2, 6, "token covers `\"first` only");
    }

    #[test]
    fn the_legend_lists_the_saved_root_type() {
        let legend = legend();
        assert!(
            legend.token_types.contains(&MARROW_SAVED_ROOT),
            "the legend must advertise the custom saved-root type"
        );
        assert_eq!(legend.token_types.len(), TOKEN_TYPES.len());
        assert_eq!(legend.token_modifiers.len(), TOKEN_MODIFIERS.len());
    }
}
