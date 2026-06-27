use marrow_check::{SymbolKind, SymbolRef};
use marrow_syntax::SourceSpan;

use super::indices::FileIndex;

pub(super) fn span_covers(span: SourceSpan, offset: usize) -> bool {
    span.start_byte <= offset && offset <= span.end_byte
}

/// The source spelling of a symbol's name, recovered from its presentation span.
pub(super) fn symbol_name(symbol: &SymbolRef, indices: &impl FileIndex) -> Option<String> {
    let line_index = indices.index_for(&symbol.file)?;
    if symbol.kind == SymbolKind::Resource {
        return first_identifier_in(
            line_index.text(),
            symbol.span.start_byte,
            symbol.span.end_byte,
        );
    }
    last_identifier_in(
        line_index.text(),
        symbol.span.start_byte,
        symbol.span.end_byte,
    )
}

fn first_identifier_in(text: &str, start: usize, end: usize) -> Option<String> {
    let end = end.min(text.len());
    if start >= end {
        return None;
    }
    let slice = &text[start..end];
    let bytes = slice.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = slice[i..].chars().next().unwrap();
        if is_ident_start(ch) {
            let run_start = i;
            while i < bytes.len() {
                let c = slice[i..].chars().next().unwrap();
                if is_ident_continue(c) {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
            let word = &slice[run_start..i];
            if !is_keyword(word) {
                return Some(word.to_string());
            }
        } else {
            i += ch.len_utf8();
        }
    }
    None
}

fn last_identifier_in(text: &str, start: usize, end: usize) -> Option<String> {
    let end = end.min(text.len());
    if start >= end {
        return None;
    }
    let slice = &text[start..end];
    let mut best: Option<&str> = None;
    let bytes = slice.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = slice[i..].chars().next().unwrap();
        if is_ident_start(ch) {
            let run_start = i;
            while i < bytes.len() {
                let c = slice[i..].chars().next().unwrap();
                if is_ident_continue(c) {
                    i += c.len_utf8();
                } else {
                    break;
                }
            }
            let word = &slice[run_start..i];
            if !is_keyword(word) {
                best = Some(word);
                let rest = slice[i..].trim_start();
                if rest.starts_with('(') || rest.starts_with(':') {
                    break;
                }
            }
        } else {
            let c = slice[i..].chars().next().unwrap();
            i += c.len_utf8();
        }
    }
    best.map(str::to_string)
}

/// The byte range of the first whole-word occurrence of `name` in `[start, end)`.
pub(super) fn name_in_span(
    text: &str,
    start: usize,
    end: usize,
    name: &str,
) -> Option<(usize, usize)> {
    let end = end.min(text.len());
    if start > end {
        return None;
    }
    let slice = text.get(start..end)?;
    let mut search_from = 0;
    while let Some(found) = slice[search_from..].find(name) {
        let abs_start = start + search_from + found;
        let abs_end = abs_start + name.len();
        if is_whole_word(text, abs_start, abs_end) {
            return Some((abs_start, abs_end));
        }
        search_from += found + 1;
        if search_from >= slice.len() {
            break;
        }
    }
    None
}

fn is_whole_word(text: &str, start: usize, end: usize) -> bool {
    let before_ok = text[..start]
        .chars()
        .next_back()
        .is_none_or(|ch| !is_ident_continue(ch));
    let after_ok = text[end..]
        .chars()
        .next()
        .is_none_or(|ch| !is_ident_continue(ch));
    before_ok && after_ok
}

fn is_ident_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_ident_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "pub" | "fn" | "const" | "var" | "resource" | "use" | "module" | "required"
    )
}
