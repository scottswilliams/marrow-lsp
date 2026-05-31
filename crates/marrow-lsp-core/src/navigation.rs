//! Go-to-definition, find-references, and rename over project analysis.
//!
//! Most navigation reads the checker's [`BindingIndex`](marrow_check::BindingIndex)
//! — the one resolution layer that maps every identifier use to the definition it
//! names, respecting lexical scope, `use` aliases, and the saved-data schema.
//! Go-to-definition also has a small syntax-backed fallback for saved roots, since
//! `^root` names durable data rather than a checker symbol. References and rename
//! stay binding-index driven, and rename applies the index's safety classification
//! so a rename that would orphan stored data is refused rather than performed.
//!
//! Spans into a file are mapped through that file's [`LineIndex`]. A caller supplies
//! the index per file (the open buffer's when present, else one built from disk),
//! mirroring how diagnostics resolve a file's position mapping.

use std::path::Path;

use lsp_types::{Location, Range, TextEdit, Url, WorkspaceEdit};
use marrow_check::{BindingIndex, RenameSafety, SymbolRef};
use marrow_syntax::{Declaration, SourceSpan, TokenKind, lex_source};

use crate::positions::LineIndex;

/// Why a rename could not be carried out, for the caller to turn into a JSON-RPC
/// error or a quiet refusal as its transport requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameError {
    /// The cursor is not on a renameable symbol.
    NoSymbol,
    /// The requested replacement is not a single valid Marrow identifier.
    InvalidName,
    /// The symbol names data encoded on disk (a saved field, group, layer, index,
    /// or a resource attached to a saved root). Renaming it in source alone would
    /// change the on-disk path and orphan the stored data, so it is refused.
    SavedDataBacked {
        /// The element's `@id(...)` stable id, when one is declared — the durable
        /// handle a migration would use to move the data across the rename.
        stable_id: Option<String>,
    },
}

impl RenameError {
    /// A human-readable explanation, used as the message a transport surfaces.
    pub fn message(&self) -> String {
        match self {
            Self::NoSymbol => "no renameable symbol at this position".to_string(),
            Self::InvalidName => "new name must be a valid Marrow identifier".to_string(),
            Self::SavedDataBacked { stable_id } => {
                let mut message = "cannot rename: this names saved data, so renaming it in \
                     source would orphan the stored records on disk"
                    .to_string();
                if let Some(id) = stable_id {
                    message.push_str(&format!(
                        " (the @id({id}) stable id lets migration tooling move the data; \
                         rename via a migration, not the editor)"
                    ));
                }
                message
            }
        }
    }
}

/// Resolves a file's [`LineIndex`] so navigation can map a span to an LSP range.
///
/// A request maps each referenced file through this: the open buffer's cached index
/// when one is held (so spans land on unsaved text, matching the editor), else an
/// index the implementation builds from disk. Returning `None` for a file drops its
/// references from the result rather than failing the whole request.
pub trait FileIndex {
    fn index_for(&self, file: &Path) -> Option<&LineIndex>;
}

/// A resolver over the files an analysis touched, holding one [`LineIndex`] per
/// file: a *borrowed* index for an open buffer (so spans map against unsaved text)
/// and an *owned* index built from disk for a file no buffer holds. Build it once
/// per navigation request from the snapshot and the open documents; the file set is
/// the project's, so the disk reads are bounded and done up front rather than mid
/// span-mapping.
pub struct SnapshotIndices<'a> {
    open: std::collections::HashMap<std::path::PathBuf, &'a LineIndex>,
    disk: std::collections::HashMap<std::path::PathBuf, LineIndex>,
}

impl<'a> SnapshotIndices<'a> {
    /// Index every file in `files`: use `open(path)`'s borrowed index when the file
    /// is an open buffer, else build one from the file's disk text.
    pub fn new(
        files: impl IntoIterator<Item = &'a Path>,
        open: impl Fn(&Path) -> Option<&'a LineIndex>,
    ) -> Self {
        let mut borrowed = std::collections::HashMap::new();
        let mut owned = std::collections::HashMap::new();
        for file in files {
            if let Some(index) = open(file) {
                borrowed.insert(file.to_path_buf(), index);
            } else {
                let text = std::fs::read_to_string(file).unwrap_or_default();
                owned.insert(file.to_path_buf(), LineIndex::new(text));
            }
        }
        Self {
            open: borrowed,
            disk: owned,
        }
    }
}

impl FileIndex for SnapshotIndices<'_> {
    fn index_for(&self, file: &Path) -> Option<&LineIndex> {
        self.open.get(file).copied().or_else(|| self.disk.get(file))
    }
}

/// The definition site of the symbol at byte `offset` in `file`, as an LSP
/// location, or `None` when the cursor is not on a known symbol. A cursor already
/// on a definition returns that definition.
pub fn definition(
    snapshot: &marrow_check::AnalysisSnapshot,
    index: &BindingIndex,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
) -> Option<Location> {
    if let Some(location) = saved_root_definition(snapshot, indices, file, offset) {
        return Some(location);
    }
    let symbol = index.definition(file, offset)?;
    symbol_location(&symbol, indices)
}

/// Every reference to the symbol at byte `offset` in `file`, as LSP locations.
/// `include_declaration` keeps or drops the definition site (the index always
/// includes it). Returns `None` when the cursor is not on a known symbol.
pub fn references(
    index: &BindingIndex,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let definition = index.definition(file, offset)?;
    let refs = index.references(&definition);
    let locations = refs
        .iter()
        .filter(|reference| include_declaration || **reference != definition)
        .filter_map(|reference| symbol_location(reference, indices))
        .collect();
    Some(locations)
}

/// The identifier range the editor should pre-select for a rename at `offset`, or
/// `None` when the cursor is not on a renameable symbol. This does not yet decide
/// rename safety — it only confirms a symbol is here and reports the span the
/// editor highlights; [`rename`] enforces the saved-data refusal when the edit is
/// requested. A `SavedDataBacked` symbol still reports its range so the editor can
/// show the refusal message against the identifier rather than silently doing
/// nothing.
pub fn prepare_rename(
    index: &BindingIndex,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
) -> Option<Range> {
    let definition = index.definition(file, offset)?;
    let name = symbol_name(&definition, indices)?;
    // The identifier under the cursor is the use span at the offset, narrowed to
    // the name. Resolve it from the file's index so the range matches the buffer.
    let line_index = indices.index_for(file)?;
    let (start, end) = name_in_span_at(line_index.text(), offset, &name)?;
    Some(line_index.range(start, end))
}

/// A workspace edit renaming the symbol at `offset` in `file` to `new_name`,
/// rewriting every reference (the declaration included). Refused with
/// [`RenameError::SavedDataBacked`] when the symbol names data encoded on disk, so
/// a rename can never silently orphan stored records.
pub fn rename(
    index: &BindingIndex,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
    new_name: &str,
) -> Result<WorkspaceEdit, RenameError> {
    if !is_valid_rename(new_name) {
        return Err(RenameError::InvalidName);
    }
    let definition = index
        .definition(file, offset)
        .ok_or(RenameError::NoSymbol)?;
    if let RenameSafety::SavedDataBacked { stable_id } = index.rename_safety(&definition) {
        return Err(RenameError::SavedDataBacked { stable_id });
    }
    let name = symbol_name(&definition, indices).ok_or(RenameError::NoSymbol)?;

    let mut edits: std::collections::HashMap<Url, Vec<TextEdit>> = std::collections::HashMap::new();
    for reference in index.references(&definition) {
        let Some(line_index) = indices.index_for(&reference.file) else {
            continue;
        };
        let Some((start, end)) = name_in_span(
            line_index.text(),
            reference.span.start_byte,
            reference.span.end_byte,
            &name,
        ) else {
            continue;
        };
        let Some(url) = Url::from_file_path(&reference.file).ok() else {
            continue;
        };
        edits.entry(url).or_default().push(TextEdit {
            range: line_index.range(start, end),
            new_text: new_name.to_string(),
        });
    }

    Ok(WorkspaceEdit {
        changes: Some(edits),
        document_changes: None,
        change_annotations: None,
    })
}

/// The LSP location of a symbol reference: the identifier within its span, mapped
/// through the file's index. A reference whose file has no resolvable index (no
/// open buffer and unreadable on disk) or whose `file://` URL cannot form yields
/// `None` and is skipped.
fn symbol_location(symbol: &SymbolRef, indices: &impl FileIndex) -> Option<Location> {
    let line_index = indices.index_for(&symbol.file)?;
    let url = Url::from_file_path(&symbol.file).ok()?;
    // Narrow to the identifier name within the span when it can be found, so a
    // definition's declaration-wide span resolves to just the name an editor jumps
    // to; otherwise the whole span stands in.
    let range = match symbol_name(symbol, indices) {
        Some(name) => {
            match name_in_span(
                line_index.text(),
                symbol.span.start_byte,
                symbol.span.end_byte,
                &name,
            ) {
                Some((start, end)) => line_index.range(start, end),
                None => line_index.range(symbol.span.start_byte, symbol.span.end_byte),
            }
        }
        None => line_index.range(symbol.span.start_byte, symbol.span.end_byte),
    };
    Some(Location { uri: url, range })
}

fn saved_root_definition(
    snapshot: &marrow_check::AnalysisSnapshot,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
) -> Option<Location> {
    let analyzed = snapshot
        .files
        .iter()
        .find(|analyzed| analyzed.path == file)?;
    let root = saved_root_at(&analyzed.source, offset)?;
    let (declaration_file, start, end) = saved_root_declaration(snapshot, &root)?;
    let line_index = indices.index_for(declaration_file)?;
    let url = Url::from_file_path(declaration_file).ok()?;
    Some(Location {
        uri: url,
        range: line_index.range(start, end),
    })
}

fn saved_root_at(source: &str, offset: usize) -> Option<String> {
    let tokens = lex_source(source).tokens;
    tokens.windows(2).find_map(|pair| {
        let [previous, token] = pair else {
            return None;
        };
        if previous.kind == TokenKind::Caret
            && token.kind == TokenKind::Identifier
            && previous.span.start_byte <= offset
            && offset <= token.span.end_byte
        {
            Some(token.text(source).to_string())
        } else {
            None
        }
    })
}

fn saved_root_declaration<'a>(
    snapshot: &'a marrow_check::AnalysisSnapshot,
    root: &str,
) -> Option<(&'a Path, usize, usize)> {
    for analyzed in &snapshot.files {
        for declaration in &analyzed.parsed.file.declarations {
            let Declaration::Resource(resource) = declaration else {
                continue;
            };
            let Some(store) = &resource.store else {
                continue;
            };
            if store.root == root {
                let (start, end) =
                    saved_root_identifier_span(&analyzed.source, resource.span, root)?;
                return Some((analyzed.path.as_path(), start, end));
            }
        }
    }
    None
}

fn saved_root_identifier_span(
    source: &str,
    span: SourceSpan,
    root: &str,
) -> Option<(usize, usize)> {
    let tokens = lex_source(source).tokens;
    tokens.windows(2).find_map(|pair| {
        let [previous, token] = pair else {
            return None;
        };
        if previous.kind == TokenKind::Caret
            && token.kind == TokenKind::Identifier
            && span_covers(span, previous.span.start_byte)
            && span_covers(span, token.span.end_byte)
            && token.text(source) == root
        {
            Some((token.span.start_byte, token.span.end_byte))
        } else {
            None
        }
    })
}

fn span_covers(span: SourceSpan, offset: usize) -> bool {
    span.start_byte <= offset && offset <= span.end_byte
}

/// The source spelling of a symbol's name: the identifier the checker resolved it
/// under, recovered from its span. A *use* of a single-segment name spans exactly
/// the identifier; a declaration span opens with a keyword, and a qualified use is
/// a `::`-separated path — in either case the name is the last identifier run in
/// the span, so scanning for it skips the leading keyword and any module qualifier.
fn symbol_name(symbol: &SymbolRef, indices: &impl FileIndex) -> Option<String> {
    let line_index = indices.index_for(&symbol.file)?;
    last_identifier_in(
        line_index.text(),
        symbol.span.start_byte,
        symbol.span.end_byte,
    )
}

/// The last keyword-free identifier within `[start, end)` of `text`, or `None` when
/// the slice holds none. For a single-token use span this is the identifier itself;
/// for a declaration (`pub fn answer(..)`) or a qualified path (`books::add`) it is
/// the declared/leaf name, since the keyword and any qualifier precede it. A
/// declaration's parameter list can follow the name, so the scan stops at the first
/// non-identifier, non-`::` separator after the name region — but taking the last
/// identifier before a `(`/`:`/`<` boundary keeps it on the name.
fn last_identifier_in(text: &str, start: usize, end: usize) -> Option<String> {
    let end = end.min(text.len());
    if start >= end {
        return None;
    }
    let slice = &text[start..end];
    // Collect identifier runs that are not keywords. For a declaration span the
    // name is the run that immediately precedes the first `(` or `:` opener; for a
    // bare or qualified use it is the only/last run. Walking runs in order and
    // keeping the last one seen before such an opener (or the last overall) lands
    // on the name in both shapes.
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
                // The name run is the one a `(` or `:` opener follows; stop there so
                // a parameter or annotation identifier does not overwrite it.
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

/// The byte range of the occurrence of `name` within `[start, end)` of `text` that
/// covers `offset`, or the first occurrence in that span. Used to map a span (a
/// declaration or qualified path) to just the identifier the cursor sits on.
fn name_in_span_at(text: &str, offset: usize, name: &str) -> Option<(usize, usize)> {
    // Scan the document around the offset for the name occurrence covering it.
    let window_start = offset.saturating_sub(name.len());
    let window_end = (offset + name.len()).min(text.len());
    let slice = text.get(window_start..window_end)?;
    let mut search_from = 0;
    while let Some(found) = slice[search_from..].find(name) {
        let start = window_start + search_from + found;
        let end = start + name.len();
        if start <= offset && offset <= end && is_whole_word(text, start, end) {
            return Some((start, end));
        }
        search_from += found + 1;
        if search_from >= slice.len() {
            break;
        }
    }
    None
}

/// The byte range of the first whole-word occurrence of `name` within
/// `[start, end)` of `text`, or `None` when it does not appear. Whole-word so a
/// rename of `id` does not touch `valid`.
fn name_in_span(text: &str, start: usize, end: usize, name: &str) -> Option<(usize, usize)> {
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

/// Whether `[start, end)` of `text` is bounded by non-identifier characters on both
/// sides, so it is a whole word rather than a substring of a longer identifier.
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

/// The keywords that open a declaration, so the first identifier in a declaration
/// span is the declared name, not the keyword. Only the handful that can lead a
/// definition span the binding index records need listing.
fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "pub" | "fn" | "const" | "var" | "resource" | "use" | "module" | "required"
    )
}

fn is_valid_rename(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    is_ident_start(first) && chars.all(is_ident_continue) && !is_reserved_word(name)
}

fn is_reserved_word(word: &str) -> bool {
    matches!(
        word,
        "and"
            | "at"
            | "bool"
            | "break"
            | "bytes"
            | "catch"
            | "const"
            | "continue"
            | "date"
            | "decimal"
            | "delete"
            | "duration"
            | "else"
            | "enum"
            | "Error"
            | "ErrorCode"
            | "false"
            | "finally"
            | "fn"
            | "for"
            | "if"
            | "in"
            | "index"
            | "inout"
            | "instant"
            | "int"
            | "lock"
            | "match"
            | "merge"
            | "module"
            | "not"
            | "or"
            | "out"
            | "pub"
            | "required"
            | "resource"
            | "return"
            | "sequence"
            | "string"
            | "throw"
            | "transaction"
            | "true"
            | "try"
            | "unique"
            | "unknown"
            | "use"
            | "var"
            | "while"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_check::{AnalysisSnapshot, ProjectSources, analyze_project, build_binding_index};
    use marrow_project::parse_config;
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// A file-index map built from in-memory text, so a test can resolve spans
    /// without touching disk. Mirrors how the server threads open buffers' indices.
    struct Indices(HashMap<PathBuf, LineIndex>);

    impl FileIndex for Indices {
        fn index_for(&self, file: &Path) -> Option<&LineIndex> {
            self.0.get(file)
        }
    }

    /// Analyze a project on disk and return the snapshot, module file paths, and
    /// file indices over the source so navigation maps spans.
    fn analyze_files(files: &[(&str, &str)]) -> (AnalysisSnapshot, Vec<PathBuf>, Indices) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let mut paths = Vec::new();
        let mut indices = HashMap::new();
        for (relative, source) in files {
            let file = src.join(relative);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&file, source).unwrap();
            indices.insert(file.clone(), LineIndex::new((*source).to_string()));
            paths.push(file);
        }
        let config = parse_config(r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let snapshot = analyze_project(root, &config, &ProjectSources::new()).unwrap();
        std::mem::forget(dir);
        (snapshot, paths, Indices(indices))
    }

    /// Analyze a one-file project on disk and return the snapshot, the module file
    /// path, and a file-index map over the source so navigation maps spans.
    fn analyze(source: &str) -> (AnalysisSnapshot, PathBuf, Indices) {
        let (snapshot, mut paths, indices) = analyze_files(&[("a.mw", source)]);
        let file = paths.pop().unwrap();
        (snapshot, file, indices)
    }

    fn offset_of(source: &str, needle: &str) -> usize {
        source.find(needle).expect("needle present")
    }

    /// The 0-based line/character of a byte offset, for asserting on a location.
    fn line_col(source: &str, offset: usize) -> (u32, u32) {
        let index = LineIndex::new(source.to_string());
        let position = index.position(offset);
        (position.line, position.character)
    }

    fn range_text<'a>(source: &'a str, index: &LineIndex, range: Range) -> &'a str {
        let start = index.offset(crate::positions::Position {
            line: range.start.line,
            character: range.start.character,
        });
        let end = index.offset(crate::positions::Position {
            line: range.end.line,
            character: range.end.character,
        });
        &source[start..end]
    }

    #[test]
    fn definition_of_a_local_use_jumps_to_its_binding() {
        let source = "module a\n\npub fn f(): int\n    const n: int = 1\n    return n\n";
        let (snapshot, file, indices) = analyze(source);
        let index = build_binding_index(&snapshot);

        // Cursor on the `n` in `return n`.
        let use_offset = offset_of(source, "return n") + "return ".len();
        let location = definition(&snapshot, &index, &indices, &file, use_offset)
            .expect("a definition for the local use");

        // It lands on the `n` in `const n`, the binding site.
        let def_offset = offset_of(source, "const n") + "const ".len();
        let (line, character) = line_col(source, def_offset);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn definition_from_saved_root_use_jumps_to_saved_root_declaration() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

pub fn f(): string
    return ^books(1).title
";
        let (snapshot, file, indices) = analyze(source);
        let index = build_binding_index(&snapshot);

        let use_offset = offset_of(source, "return ^books") + "return ^".len();
        let location = definition(&snapshot, &index, &indices, &file, use_offset)
            .expect("a saved-root use resolves to its declaration");
        let line_index = indices.0.get(&file).unwrap();

        assert_eq!(range_text(source, line_index, location.range), "books");
        let declaration = offset_of(source, "resource Book at ^books") + "resource Book at ^".len();
        let (line, character) = line_col(source, declaration);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn definition_from_saved_root_use_works_on_the_caret() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

pub fn f(): string
    return ^books(1).title
";
        let (snapshot, file, indices) = analyze(source);
        let index = build_binding_index(&snapshot);

        let use_offset = offset_of(source, "return ^books") + "return ".len();
        let location = definition(&snapshot, &index, &indices, &file, use_offset)
            .expect("a saved-root caret resolves to its declaration");
        let line_index = indices.0.get(&file).unwrap();

        assert_eq!(range_text(source, line_index, location.range), "books");
        let declaration = offset_of(source, "resource Book at ^books") + "resource Book at ^".len();
        let (line, character) = line_col(source, declaration);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn definition_from_saved_root_declaration_selects_root_identifier_not_resource_name() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

pub fn f(): string
    return ^books(1).title
";
        let (snapshot, file, indices) = analyze(source);
        let index = build_binding_index(&snapshot);

        let declaration = offset_of(source, "resource Book at ^books") + "resource Book at ^".len();
        let location = definition(&snapshot, &index, &indices, &file, declaration + 1)
            .expect("a saved-root declaration resolves to itself");
        let line_index = indices.0.get(&file).unwrap();

        assert_eq!(range_text(source, line_index, location.range), "books");
        let (line, character) = line_col(source, declaration);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn definition_from_saved_root_declaration_works_on_the_caret() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

pub fn f(): string
    return ^books(1).title
";
        let (snapshot, file, indices) = analyze(source);
        let index = build_binding_index(&snapshot);

        let declaration_caret =
            offset_of(source, "resource Book at ^books") + "resource Book at ".len();
        let location = definition(&snapshot, &index, &indices, &file, declaration_caret)
            .expect("a saved-root declaration caret resolves to itself");
        let line_index = indices.0.get(&file).unwrap();

        assert_eq!(range_text(source, line_index, location.range), "books");
        let declaration = declaration_caret + "^".len();
        let (line, character) = line_col(source, declaration);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn references_of_a_param_returns_all_its_uses_shadowing_correct() {
        // The parameter `n` is used twice; an inner block shadows it with a local
        // `n` whose use must NOT be attributed to the parameter.
        let source = "\
module a

pub fn f(n: int): int
    if true
        const n: int = 99
        return n
    return n
";
        let (snapshot, file, indices) = analyze(source);
        let index = build_binding_index(&snapshot);

        // Query from the outer use of the parameter (the realistic editor case: the
        // cursor sits on an identifier use). A parameter has no AST span of its own,
        // so resolving *from its declaration* would land on the enclosing function;
        // resolving from a use lands on the parameter binding.
        let outer_return = source.rfind("return n").unwrap() + "return ".len();
        let refs = references(&index, &indices, &file, outer_return, true)
            .expect("references for the parameter");
        let lines: Vec<u32> = refs.iter().map(|r| r.range.start.line).collect();

        // The outer use is among the references.
        let (outer_line, _) = line_col(source, outer_return);
        assert!(
            lines.contains(&outer_line),
            "the outer use of the parameter should be a reference, got {lines:?}"
        );

        // The inner `return n` is shadowed by the inner `const n`, so it must NOT be
        // attributed to the parameter.
        let inner_return = source.find("return n").unwrap() + "return ".len();
        let (inner_line, _) = line_col(source, inner_return);
        assert!(
            !lines.contains(&inner_line),
            "the shadowed inner use must NOT be a reference of the parameter, got {lines:?}"
        );
    }

    #[test]
    fn references_can_exclude_the_declaration() {
        let source = "module a\n\npub fn f(n: int): int\n    return n\n";
        let (snapshot, file, indices) = analyze(source);
        let index = build_binding_index(&snapshot);

        // Query from the use so the parameter binding resolves (its declaration has
        // no span of its own). `with` keeps the declaration; `without` drops it.
        let use_offset = offset_of(source, "return n") + "return ".len();
        let with = references(&index, &indices, &file, use_offset, true).unwrap();
        let without = references(&index, &indices, &file, use_offset, false).unwrap();
        assert_eq!(
            with.len(),
            without.len() + 1,
            "excluding the declaration drops exactly one location"
        );
    }

    #[test]
    fn rename_of_a_local_rewrites_all_uses() {
        let source = "module a\n\npub fn f(): int\n    const n: int = 1\n    return n + n\n";
        let (snapshot, file, indices) = analyze(source);
        let index = build_binding_index(&snapshot);

        let def_offset = offset_of(source, "const n") + "const ".len();
        let edit =
            rename(&index, &indices, &file, def_offset, "count").expect("a local rename succeeds");

        let url = Url::from_file_path(&file).unwrap();
        let changes = edit.changes.expect("changes present");
        let edits = &changes[&url];
        // The declaration `n` plus its two uses in `n + n`: three edits.
        assert_eq!(edits.len(), 3, "every occurrence of the local is rewritten");
        for text_edit in edits {
            assert_eq!(text_edit.new_text, "count");
        }
    }

    #[test]
    fn rename_of_a_saved_field_is_refused() {
        let source = "\
module a

resource Book at ^books(id: int)
    required title: string

pub fn f(): string
    return ^books(1).title
";
        let (snapshot, file, indices) = analyze(source);
        let index = build_binding_index(&snapshot);

        // Cursor on the `title` field declaration.
        let field_offset = offset_of(source, "required title") + "required ".len();
        let result = rename(&index, &indices, &file, field_offset, "name");
        assert!(
            matches!(result, Err(RenameError::SavedDataBacked { .. })),
            "renaming a saved field must be refused, got {result:?}"
        );
    }

    #[test]
    fn rename_off_any_symbol_is_no_symbol() {
        let source = "module a\n\npub fn f(): int\n    return 1\n";
        let (snapshot, file, indices) = analyze(source);
        let index = build_binding_index(&snapshot);
        // Offset 0 is the `module` keyword — no symbol.
        let result = rename(&index, &indices, &file, 0, "x");
        assert_eq!(result, Err(RenameError::NoSymbol));
    }

    #[test]
    fn rename_rejects_invalid_replacement_text() {
        let source = "module a\n\npub fn f(): int\n    const n: int = 1\n    return n\n";
        let (snapshot, file, indices) = analyze(source);
        let index = build_binding_index(&snapshot);
        let def_offset = offset_of(source, "const n") + "const ".len();

        assert_eq!(
            rename(&index, &indices, &file, def_offset, "two words"),
            Err(RenameError::InvalidName)
        );
        assert_eq!(
            rename(&index, &indices, &file, def_offset, "return"),
            Err(RenameError::InvalidName)
        );
        // Marrow keywords are not valid identifiers; renaming a symbol to one
        // would produce source that no longer parses.
        for keyword in ["at", "index", "unique", "out", "inout", "ErrorCode"] {
            assert_eq!(
                rename(&index, &indices, &file, def_offset, keyword),
                Err(RenameError::InvalidName),
                "renaming to the reserved word `{keyword}` must be rejected"
            );
        }
    }

    #[test]
    fn prepare_rename_returns_the_identifier_range() {
        let source = "module a\n\npub fn f(): int\n    const n: int = 1\n    return n\n";
        let (snapshot, file, indices) = analyze(source);
        let index = build_binding_index(&snapshot);

        let use_offset = offset_of(source, "return n") + "return ".len();
        let range = prepare_rename(&index, &indices, &file, use_offset)
            .expect("a renameable identifier at the use");
        let (line, character) = line_col(source, use_offset);
        assert_eq!(range.start.line, line);
        assert_eq!(range.start.character, character);
        assert_eq!(range.end.character, character + 1, "the range covers `n`");
    }

    #[test]
    fn definition_from_enum_type_annotation_jumps_to_enum_name() {
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
        let (snapshot, paths, indices) =
            analyze_files(&[("a/b.mw", status_source), ("app.mw", app_source)]);
        let status_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let annotation = offset_of(app_source, "b::Status") + "b::".len();
        let location = definition(&snapshot, &index, &indices, app_file, annotation + 1)
            .expect("enum annotation resolves to its declaration");
        let line_index = indices.0.get(status_file).unwrap();

        assert_eq!(
            range_text(status_source, line_index, location.range),
            "Status"
        );
        let enum_name = offset_of(status_source, "enum Status") + "enum ".len();
        let (line, character) = line_col(status_source, enum_name);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn references_from_enum_type_annotation_stay_on_that_enum() {
        let status_source = "\
module a::b
pub enum Status
    active
    archived
";
        let app_source = "\
module app
use a::b

enum Color
    active

fn f(): b::Status
    const current: b::Status = b::Status::active
    const color: Color = Color::active
    return current
";
        let (snapshot, paths, indices) =
            analyze_files(&[("a/b.mw", status_source), ("app.mw", app_source)]);
        let status_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let annotation =
            offset_of(app_source, "const current: b::Status") + "const current: b::".len();
        let refs = references(&index, &indices, app_file, annotation + 1, true)
            .expect("references for enum annotation");
        let texts: Vec<&str> = refs
            .iter()
            .map(|location| {
                let path = location.uri.to_file_path().unwrap();
                let (source, line_index) = if path == *status_file {
                    (status_source, indices.0.get(status_file).unwrap())
                } else {
                    (app_source, indices.0.get(app_file).unwrap())
                };
                range_text(source, line_index, location.range)
            })
            .collect();

        assert_eq!(
            texts,
            vec!["Status", "Status", "Status", "Status"],
            "declaration, return annotation, local annotation, and literal prefix are enum refs"
        );

        let without_decl = references(&index, &indices, app_file, annotation + 1, false).unwrap();
        assert_eq!(
            without_decl.len(),
            refs.len() - 1,
            "excluding the declaration drops only the enum declaration"
        );
        assert!(
            texts
                .iter()
                .all(|text| *text != "active" && *text != "Color"),
            "enum references should not include member values or another enum: {texts:?}"
        );
    }

    #[test]
    fn rename_from_enum_type_annotation_rewrites_leaf_enum_occurrences_only() {
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
    const current: b::Status = b::Status::active
    return current
";
        let (snapshot, paths, indices) =
            analyze_files(&[("a/b.mw", status_source), ("app.mw", app_source)]);
        let status_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);
        let app_index = indices.0.get(app_file).unwrap();

        let annotation =
            offset_of(app_source, "const current: b::Status") + "const current: b::".len();
        let prepared = prepare_rename(&index, &indices, app_file, annotation + 1)
            .expect("enum annotation can prepare rename");
        assert_eq!(range_text(app_source, app_index, prepared), "Status");

        let edit = rename(&index, &indices, app_file, annotation + 1, "State")
            .expect("enum annotation rename succeeds");
        let changes = edit.changes.expect("changes present");
        let mut replaced = Vec::new();
        for (url, edits) in &changes {
            let path = url.to_file_path().unwrap();
            let (source, line_index) = if path == *status_file {
                (status_source, indices.0.get(status_file).unwrap())
            } else {
                (app_source, app_index)
            };
            for edit in edits {
                replaced.push(range_text(source, line_index, edit.range));
                assert_eq!(edit.new_text, "State");
            }
        }
        replaced.sort_unstable();

        assert_eq!(
            replaced,
            vec!["Status", "Status", "Status", "Status"],
            "rename touches the declaration, annotations, and literal enum prefix"
        );
    }
}
