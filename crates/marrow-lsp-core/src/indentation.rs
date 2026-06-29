use lsp_types::{FormattingOptions, Position, Range, TextEdit};
use marrow_syntax::{Keyword, LexedSource, TokenKind};

pub fn newline_indentation(
    text: &str,
    lexed: &LexedSource,
    position: Position,
    options: &FormattingOptions,
) -> Option<Vec<TextEdit>> {
    let previous_line = position.line.checked_sub(1)?;
    let (previous_start, previous_end) = line_bounds(text, previous_line)?;
    let (current_start, current_end) = line_bounds(text, position.line)?;
    let previous_indent = leading_indent(text, previous_start, previous_end);
    let current_indent_end = leading_indent_end(text, current_start, current_end);
    let current_indent = &text[current_start..current_indent_end];

    let significant_tokens = significant_line_tokens(lexed, previous_start, previous_end);
    let desired = if significant_tokens.is_empty() {
        previous_indent.to_string()
    } else if is_terminator(&significant_tokens) {
        outdented(previous_indent, options)
    } else if is_block_starter(&significant_tokens) {
        let mut indent = previous_indent.to_string();
        indent.push_str(&indent_unit(options));
        indent
    } else {
        previous_indent.to_string()
    };

    if current_indent == desired {
        return None;
    }

    Some(vec![TextEdit {
        range: Range {
            start: Position::new(position.line, 0),
            end: Position::new(position.line, (current_indent_end - current_start) as u32),
        },
        new_text: desired,
    }])
}

fn line_bounds(text: &str, line: u32) -> Option<(usize, usize)> {
    let mut start = 0;
    for _ in 0..line {
        let newline = text[start..].find('\n')?;
        start += newline + 1;
    }
    let mut end = text[start..]
        .find('\n')
        .map_or(text.len(), |newline| start + newline);
    if end > start && text.as_bytes()[end - 1] == b'\r' {
        end -= 1;
    }
    Some((start, end))
}

fn leading_indent(text: &str, start: usize, end: usize) -> &str {
    &text[start..leading_indent_end(text, start, end)]
}

fn leading_indent_end(text: &str, start: usize, end: usize) -> usize {
    start
        + text[start..end]
            .bytes()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count()
}

fn significant_line_tokens(
    lexed: &LexedSource,
    line_start: usize,
    line_end: usize,
) -> Vec<TokenKind> {
    lexed
        .tokens
        .iter()
        .filter(|token| line_start <= token.span.start_byte && token.span.start_byte < line_end)
        .filter(|token| !is_trivia(token.kind))
        .map(|token| token.kind)
        .collect()
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

fn is_block_starter(tokens: &[TokenKind]) -> bool {
    matches!(
        tokens,
        [
            TokenKind::Keyword(Keyword::Pub),
            TokenKind::Keyword(Keyword::Fn | Keyword::Enum),
            ..
        ] | [
            TokenKind::Keyword(
                Keyword::Fn
                    | Keyword::Resource
                    | Keyword::Enum
                    | Keyword::Surface
                    | Keyword::Evolve
                    | Keyword::If
                    | Keyword::Else
                    | Keyword::For
                    | Keyword::While
                    | Keyword::Match
                    | Keyword::Transaction
                    | Keyword::Try
                    | Keyword::Catch
            ),
            ..
        ]
    )
}

fn is_terminator(tokens: &[TokenKind]) -> bool {
    matches!(
        tokens,
        [
            TokenKind::Keyword(
                Keyword::Return | Keyword::Throw | Keyword::Break | Keyword::Continue
            ),
            ..
        ]
    )
}

fn indent_unit(options: &FormattingOptions) -> String {
    if options.insert_spaces {
        " ".repeat(tab_size(options))
    } else {
        "\t".to_string()
    }
}

fn tab_size(options: &FormattingOptions) -> usize {
    if options.tab_size == 0 {
        4
    } else {
        options.tab_size as usize
    }
}

fn outdented(indent: &str, options: &FormattingOptions) -> String {
    if indent.is_empty() {
        return String::new();
    }
    if !options.insert_spaces
        && let Some(stripped) = indent.strip_suffix('\t')
    {
        return stripped.to_string();
    }

    let mut cut = indent.len();
    let mut remaining = tab_size(options);
    while remaining > 0 && cut > 0 {
        match indent.as_bytes()[cut - 1] {
            b' ' => {
                cut -= 1;
                remaining -= 1;
            }
            b'\t' => {
                cut -= 1;
                break;
            }
            _ => break,
        }
    }
    indent[..cut].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Range;
    use marrow_syntax::lex_source;

    fn spaces_options() -> FormattingOptions {
        FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            ..FormattingOptions::default()
        }
    }

    fn tabs_options() -> FormattingOptions {
        FormattingOptions {
            tab_size: 4,
            insert_spaces: false,
            ..FormattingOptions::default()
        }
    }

    fn edit(source: &str, line: u32, options: FormattingOptions) -> Option<TextEdit> {
        let lexed = lex_source(source);
        newline_indentation(source, &lexed, Position::new(line, 0), &options).map(|mut edits| {
            assert_eq!(
                edits.len(),
                1,
                "indentation returns one leading-whitespace edit"
            );
            edits.remove(0)
        })
    }

    fn assert_indent_edit(
        source: &str,
        line: u32,
        replaced_characters: u32,
        expected_indent: &str,
        options: FormattingOptions,
    ) {
        let edit = edit(source, line, options).expect("indentation should edit the current line");
        assert_eq!(
            edit.range,
            Range {
                start: Position::new(line, 0),
                end: Position::new(line, replaced_characters),
            }
        );
        assert_eq!(edit.new_text, expected_indent);
    }

    #[test]
    fn module_line_does_not_indent() {
        assert!(
            edit("module app\nnext", 1, spaces_options()).is_none(),
            "module headers do not start indentation blocks"
        );
    }

    #[test]
    fn function_header_indents_body() {
        assert_indent_edit("pub fn f()\nbody", 1, 0, "    ", spaces_options());
    }

    #[test]
    fn pub_enum_header_indents_body() {
        assert_indent_edit("pub enum Status\nactive", 1, 0, "    ", spaces_options());
    }

    #[test]
    fn if_header_indents_body() {
        assert_indent_edit(
            "pub fn f()\n    if ready\nnext",
            2,
            0,
            "        ",
            spaces_options(),
        );
    }

    #[test]
    fn transaction_header_indents_body() {
        assert_indent_edit(
            "pub fn f()\n    transaction\nnext",
            2,
            0,
            "        ",
            spaces_options(),
        );
    }

    #[test]
    fn store_header_does_not_indent_because_store_body_is_optional() {
        assert_indent_edit(
            "store ^books(id: int): Book\n    next",
            1,
            4,
            "",
            spaces_options(),
        );
    }

    #[test]
    fn return_outdents_next_line() {
        assert_indent_edit(
            "pub fn f()\n    return\n        next",
            2,
            8,
            "",
            spaces_options(),
        );
    }

    #[test]
    fn throw_break_continue_outdent_next_line() {
        for keyword in ["throw error", "break", "continue"] {
            let source = format!("pub fn f()\n        {keyword}\n            next");
            assert_indent_edit(&source, 2, 12, "    ", spaces_options());
        }
    }

    #[test]
    fn generic_call_line_does_not_indent() {
        assert_indent_edit("thing(value)\n    next", 1, 4, "", spaces_options());
    }

    #[test]
    fn blank_previous_line_keeps_current_indentation() {
        assert!(
            edit("pub fn f()\n    \n    next", 2, spaces_options()).is_none(),
            "blank lines preserve the indentation the editor already copied"
        );
    }

    #[test]
    fn comment_previous_line_keeps_current_indentation() {
        assert!(
            edit("pub fn f()\n    ; comment\n    next", 2, spaces_options()).is_none(),
            "comment-only lines preserve the indentation the editor already copied"
        );
    }

    #[test]
    fn tabs_are_used_when_insert_spaces_is_false() {
        assert_indent_edit("pub fn f()\nbody", 1, 0, "\t", tabs_options());
    }
}
