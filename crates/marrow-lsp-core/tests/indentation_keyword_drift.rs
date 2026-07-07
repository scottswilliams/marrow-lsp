//! Drift gate for on-type indentation's keyword dependency (roadmap S0.4).
//!
//! On-type indentation (`indentation.rs`) is a `presentation-only` typing aid: it classifies
//! re-lexed keywords into block-openers, a visibility keyword, and block-terminators. That
//! classification is a *local* mirror of the marrow grammar, so it silently drifts if the
//! grammar's keyword set changes. This gate pins the classification against the full
//! `marrow_syntax::Keyword` enum: the match is exhaustive, so a new or renamed keyword stops
//! this test from compiling until the keyword is deliberately classified — the loud signal
//! that indentation may need to handle it. Graduating indentation to `ready` (consuming a
//! marrow parser block-structure fact, UPSTREAM-6) retires this gate.
//!
//! This intentionally re-encodes the classification rather than importing `indentation.rs`'s
//! (unexposed) logic: it gates against *grammar* drift (a new or renamed keyword), not against
//! edits to `indentation.rs` itself. That is the right scope for a `presentation-only` aid.

use marrow_syntax::Keyword;

#[derive(Debug, PartialEq, Eq)]
enum IndentRole {
    /// Opens an indented block: a newline after its line increases indent.
    BlockOpener,
    /// A visibility modifier that precedes a block opener (`pub fn`, `pub enum`).
    Visibility,
    /// Ends a statement path: no deeper indent follows it.
    Terminator,
    /// Not relevant to indentation.
    Other,
}

/// Classify every keyword the way on-type indentation depends on. Exhaustive on purpose.
fn indent_role(keyword: Keyword) -> IndentRole {
    use Keyword::*;
    match keyword {
        Pub => IndentRole::Visibility,
        Fn | Resource | Enum | Surface | Evolve | If | Else | For | While | Match | Transaction
        | Try | Catch => IndentRole::BlockOpener,
        Return | Throw | Break | Continue => IndentRole::Terminator,
        Module | Use | Store | Index | Unique | Required | Const | Var | In | Absent | Delete
        | Merge | Journal | Sensitive | Declassify | Lock | True | False | Not | And | Or | Is
        | Int | Decimal | Bool | String | Bytes | Date | Instant | Duration | Sequence
        | Unknown | Error | ErrorCode | Id => IndentRole::Other,
    }
}

#[test]
fn indentation_block_and_visibility_keywords_are_pinned() {
    let block_openers = [
        Keyword::Fn,
        Keyword::Resource,
        Keyword::Enum,
        Keyword::Surface,
        Keyword::Evolve,
        Keyword::If,
        Keyword::Else,
        Keyword::For,
        Keyword::While,
        Keyword::Match,
        Keyword::Transaction,
        Keyword::Try,
        Keyword::Catch,
    ];
    for keyword in block_openers {
        assert_eq!(
            indent_role(keyword),
            IndentRole::BlockOpener,
            "{keyword:?} is expected to open an indented block"
        );
    }

    assert_eq!(indent_role(Keyword::Pub), IndentRole::Visibility);

    for keyword in [
        Keyword::Return,
        Keyword::Throw,
        Keyword::Break,
        Keyword::Continue,
    ] {
        assert_eq!(indent_role(keyword), IndentRole::Terminator);
    }
}
