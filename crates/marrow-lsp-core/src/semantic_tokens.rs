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

use lsp_types::{SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend};
use marrow_syntax::{
    Declaration, EnumMember, FieldDecl, GroupDecl, IndexDecl, Keyword, LexedSource, ResourceMember,
    SourceFile, SourceSpan, Token, TokenKind,
};

use crate::positions::LineIndex;

/// A saved-data root (`^books`): the value type the editor's grammar cannot tell
/// from a local variable. Not in the LSP's standard set, so it is named here and
/// the legend advertises it as a custom type a theme can color.
const MARROW_SAVED_ROOT: SemanticTokenType = SemanticTokenType::new("savedRoot");

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
];

/// The token modifiers this server emits. Only the saved-root sigil and name use
/// one (`MODIFICATION`), marking them as data the language mutates in place.
const TOKEN_MODIFIERS: &[SemanticTokenModifier] = &[SemanticTokenModifier::MODIFICATION];

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

/// The one modifier bit, `MODIFICATION` (index 0).
const MOD_MODIFICATION: u32 = 1 << 0;

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
    let mut tokens = Vec::new();
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;
    let overrides = declaration_overrides(lexed, file, index.text());

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

        if let Some(ty) = overrides
            .get(&(token.span.start_byte, token.span.end_byte))
            .copied()
            .or_else(|| token_type(token.kind))
        {
            push(
                &mut tokens,
                token,
                ty,
                0,
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
                if let Some(store) = &resource.store
                    && !store.keys.is_empty()
                {
                    add_saved_root_key_overrides(
                        &mut overrides,
                        lexed,
                        source,
                        resource.span,
                        &store.root,
                        store.keys.iter().map(|key| key.name.as_str()),
                    );
                }
                for member in &resource.members {
                    add_resource_member_overrides(&mut overrides, lexed, source, member);
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
        ResourceMember::Index(index) => add_index_overrides(overrides, lexed, source, index),
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
    add_first_identifier_override(
        overrides,
        lexed,
        source,
        index.span,
        &index.name,
        TYPE_PROPERTY,
    );
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

/// The semantic token type for a lexer token kind, or `None` for trivia and
/// structural tokens the editor does not color (indentation, newlines, EOF,
/// parentheses, commas).
fn token_type(kind: TokenKind) -> Option<u32> {
    match kind {
        TokenKind::Identifier => Some(TYPE_VARIABLE),
        TokenKind::Integer | TokenKind::Decimal => Some(TYPE_NUMBER),
        // The whole string — including a `$"...{x}..."` interpolation's literal
        // segments and braces — colors as a string; the interpolated expression
        // tokens between them are lexed as their own kinds and color normally.
        TokenKind::String
        | TokenKind::Bytes
        | TokenKind::InterpolationStart
        | TokenKind::InterpolationText
        | TokenKind::InterpolationEnd => Some(TYPE_STRING),
        TokenKind::Comment | TokenKind::DocComment => Some(TYPE_COMMENT),
        TokenKind::Keyword(keyword) => Some(if is_type_keyword(keyword) {
            TYPE_TYPE
        } else {
            TYPE_KEYWORD
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
        | TokenKind::At => Some(TYPE_OPERATOR),
        // The `^` sigil is handled before this is reached; structural delimiters
        // and the interpolation-expression braces are left to the grammar.
        _ => None,
    }
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

    fn decoded_for(source: &str) -> (LineIndex, DecodedTokens) {
        let index = LineIndex::new(source);
        let parsed = parse_source(source);
        let decoded = absolute(&semantic_tokens(&lex_source(source), &parsed.file, &index));
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
