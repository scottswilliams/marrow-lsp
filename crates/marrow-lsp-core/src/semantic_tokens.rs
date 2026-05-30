//! Classify a cached lex into LSP semantic tokens.
//!
//! The server registers a [`legend`] at initialize and answers
//! `textDocument/semanticTokens/full` by mapping the document's cached
//! [`LexedSource`] — never a re-lex — into the LSP's delta-encoded token stream.
//! The lexer is error-recovering, so this produces tokens even while the buffer
//! has errors.
//!
//! The one distinction the editor's own grammar cannot draw lives here: a
//! saved-root sigil `^` and the root name that follows it are a dedicated token
//! type (`MARROW_SAVED_ROOT`) with the `MODIFICATION` modifier, so a durable
//! `^books` reads differently from a local variable named `books`.

use lsp_types::{SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend};
use marrow_syntax::{Keyword, LexedSource, Token, TokenKind};

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
pub fn semantic_tokens(lexed: &LexedSource, index: &LineIndex) -> Vec<SemanticToken> {
    let mut tokens = Vec::new();
    let mut prev_line = 0u32;
    let mut prev_start = 0u32;

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

        if let Some(ty) = token_type(token.kind) {
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
    use marrow_syntax::lex_source;

    /// Decode the delta-encoded tokens back to absolute `(line, character, length,
    /// type, modifiers)` tuples so a test can assert on positions directly.
    fn absolute(tokens: &[SemanticToken]) -> Vec<(u32, u32, u32, u32, u32)> {
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

    #[test]
    fn saved_root_sigil_and_name_are_marked_distinctly_from_a_variable() {
        // `^books` reads/writes saved data; `books` is just a local. They must not
        // carry the same token type.
        let source = "module a\n\npub fn f()\n    const books: int = 1\n    delete ^books(books)\n";
        let index = LineIndex::new(source);
        let tokens = semantic_tokens(&lex_source(source), &index);
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
        let decoded = absolute(&semantic_tokens(&lex_source(source), &index));
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
        let decoded = absolute(&semantic_tokens(&lex_source(source), &index));
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
