use marrow_syntax::{Keyword, SourceSpan, Token, TokenKind};

use super::{
    TYPE_BOOLEAN_LITERAL, TYPE_COMMENT, TYPE_KEYWORD, TYPE_NAMESPACE, TYPE_NUMBER, TYPE_OPERATOR,
    TYPE_STRING, TYPE_TYPE, TYPE_VARIABLE,
};

pub(super) fn token_in_span(token: &Token, span: SourceSpan) -> bool {
    token.span.start_byte >= span.start_byte && token.span.end_byte <= span.end_byte
}

pub(super) fn is_path_segment_token(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::Identifier | TokenKind::Keyword(_))
}

pub(super) fn is_trivia(kind: TokenKind) -> bool {
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
/// structural tokens the editor does not color.
pub(super) fn token_type(kind: TokenKind) -> Option<u32> {
    match kind {
        TokenKind::Identifier => Some(TYPE_VARIABLE),
        TokenKind::Integer | TokenKind::Decimal | TokenKind::Duration => Some(TYPE_NUMBER),
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
        _ => None,
    }
}

fn is_operator_keyword(keyword: Keyword) -> bool {
    matches!(
        keyword,
        Keyword::Not | Keyword::And | Keyword::Or | Keyword::Is
    )
}

/// Whether a keyword names a type so it colors as a type, not a control word.
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
