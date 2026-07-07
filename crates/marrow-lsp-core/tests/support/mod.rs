//! Shared source-splitting helpers for the workspace architecture gates.
//!
//! The structural gates (`store_seam.rs`, `architecture_gate.rs`) assert invariants over
//! *production* Rust only: a `#[cfg(test)] mod tests { ... }` block legitimately constructs
//! stores, lexes tokens, and lists forbidden symbols as string literals, so it must be stripped
//! before any symbol scan. Brace depth — comment/string/char aware — finds each test module's
//! matching close so the split does not desync on `{`/`}` inside literals.

use std::path::{Path, PathBuf};

/// Net brace depth contributed by a line, ignoring `{`/`}` inside line comments, string
/// literals, and char literals. Char literals are distinguished from lifetimes so a `&'a T {`
/// on one line is not misread. Block comments and raw strings are not modelled; they do not
/// appear in the scanned test modules.
fn brace_delta(line: &str) -> i32 {
    enum State {
        Code,
        Str,
        Char,
    }
    let mut state = State::Code;
    let mut delta = 0;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match state {
            State::Code => match c {
                '/' if chars.peek() == Some(&'/') => break,
                '"' => state = State::Str,
                '\'' => {
                    // A char literal is `'\<esc>` or `'<char>'`; anything else is a lifetime.
                    let mut look = chars.clone();
                    let is_char_literal = look.peek() == Some(&'\\')
                        || (look.next().is_some() && look.peek() == Some(&'\''));
                    if is_char_literal {
                        state = State::Char;
                    }
                }
                '{' => delta += 1,
                '}' => delta -= 1,
                _ => {}
            },
            State::Str => match c {
                '\\' => {
                    chars.next();
                }
                '"' => state = State::Code,
                _ => {}
            },
            State::Char => match c {
                '\\' => {
                    chars.next();
                }
                '\'' => state = State::Code,
                _ => {}
            },
        }
    }
    delta
}

/// Strip `#[cfg(test)] mod tests { ... }` blocks so only production source remains. Brace depth
/// (comment/string/char-aware, see [`brace_delta`]) finds each test module's matching close.
pub fn production_source(text: &str) -> String {
    let mut out = String::new();
    let mut depth: i32 = 0;
    let mut test_mod_open_depth: Option<i32> = None;
    let mut prev_line_was_cfg_test = false;

    for line in text.lines() {
        let trimmed = line.trim_start();
        if test_mod_open_depth.is_none() && prev_line_was_cfg_test && trimmed.starts_with("mod ") {
            test_mod_open_depth = Some(depth);
        }
        if test_mod_open_depth.is_none() {
            out.push_str(line);
            out.push('\n');
        }
        depth += brace_delta(line);
        if let Some(open_depth) = test_mod_open_depth
            && depth <= open_depth
        {
            test_mod_open_depth = None;
        }
        prev_line_was_cfg_test = trimmed == "#[cfg(test)]";
    }
    out
}

/// Collect every `.rs` file under `dir`, skipping `tests.rs` submodule files: those hold only
/// `#[cfg(test)]` code (they are pulled in via `#[cfg(test)] mod tests;`), including the
/// forbidden-symbol string literals the gates search for, so scanning them as production is wrong.
pub fn production_rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            production_rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && path.file_name().is_none_or(|name| name != "tests.rs")
        {
            out.push(path);
        }
    }
}
