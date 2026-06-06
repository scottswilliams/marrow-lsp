//! Go-to-definition, find-references, and rename over project analysis.
//!
//! Most navigation reads the checker's [`BindingIndex`](marrow_check::BindingIndex)
//! — the one resolution layer that maps every identifier use to the definition it
//! names, respecting lexical scope, `use` aliases, and the saved-data schema.
//! Module and use path segments stay unavailable until Marrow exposes canonical
//! module-path facts. References and rename stay binding-index driven, and rename
//! applies the index's safety classification so a rename that would orphan stored
//! data is refused rather than performed.
//!
//! Spans into a file are mapped through that file's [`LineIndex`]. A caller supplies
//! the index per file (the open buffer's when present, else one built from disk),
//! mirroring how diagnostics resolve a file's position mapping.

use std::path::Path;

use lsp_types::{Location, Range, TextEdit, Url, WorkspaceEdit};
use marrow_check::{
    BindingIndex, DefItem, RenameSafety, Resolution, ResolvableKind, SymbolKind, SymbolRef, resolve,
};
use marrow_syntax::{
    Declaration, EvolveStep, Keyword, ResourceMember, SourceFile, SourceSpan, Statement, Token,
    TokenKind, TypeRef, lex_source,
};

use crate::positions::LineIndex;

/// Why a rename could not be carried out, for the caller to turn into a JSON-RPC
/// error or a quiet refusal as its transport requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameError {
    /// The cursor is not on a renameable symbol.
    NoSymbol,
    /// The requested replacement is not a single valid Marrow identifier.
    InvalidName,
    /// The symbol names data encoded on disk (a store root, store-backed resource,
    /// saved field, group, layer, or index). Renaming it in source alone would change
    /// the on-disk path and orphan the stored data, so it is refused.
    SavedDataBacked,
}

impl RenameError {
    /// A human-readable explanation, used as the message a transport surfaces.
    pub fn message(&self) -> String {
        match self {
            Self::NoSymbol => "no renameable symbol at this position".to_string(),
            Self::InvalidName => "new name must be a valid Marrow identifier".to_string(),
            Self::SavedDataBacked => "cannot rename: this names saved data, so renaming it in \
                 source would orphan the stored records on disk"
                .to_string(),
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
    match module_path_definition(snapshot, index, indices, file, offset) {
        Some(ModulePathDefinition::Location(location)) => return Some(location),
        Some(ModulePathDefinition::NoDefinition) => return None,
        None => {}
    }
    // Go-to-definition for saved roots needs its own checked store-root fact, so
    // this path stays quiet until Marrow exposes one.
    if saved_root_syntax_at(snapshot, file, offset) {
        return None;
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
    if let RenameSafety::SavedDataBacked = index.rename_safety(&definition) {
        return Err(RenameError::SavedDataBacked);
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

fn saved_root_syntax_at(
    snapshot: &marrow_check::AnalysisSnapshot,
    file: &Path,
    offset: usize,
) -> bool {
    let Some(analyzed) = snapshot.files.iter().find(|analyzed| analyzed.path == file) else {
        return false;
    };
    let tokens = lex_source(&analyzed.source).tokens;
    tokens.windows(2).any(|pair| {
        let [previous, token] = pair else {
            return false;
        };
        previous.kind == TokenKind::Caret
            && token.kind == TokenKind::Identifier
            && previous.span.start_byte <= offset
            && offset <= token.span.end_byte
    })
}

fn span_covers(span: SourceSpan, offset: usize) -> bool {
    span.start_byte <= offset && offset <= span.end_byte
}

fn module_path_definition(
    snapshot: &marrow_check::AnalysisSnapshot,
    index: &BindingIndex,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
) -> Option<ModulePathDefinition> {
    let analyzed = snapshot
        .files
        .iter()
        .find(|analyzed| analyzed.path == file)?;
    let in_type_annotation = type_annotation_at(&analyzed.parsed.file, offset);
    let path = qualified_path_at(&analyzed.source, offset, in_type_annotation)?;
    let function_location = if path.declaration.is_none() && !in_type_annotation {
        function_path_definition(snapshot, indices, file, &path)
    } else {
        None
    };
    if path.declaration.is_none()
        && let Some(symbol) = index.definition(file, offset)
        && path.binding_index_should_win(symbol.kind)
    {
        return None;
    }
    match path.declaration {
        Some(ModulePathDeclaration::Module | ModulePathDeclaration::Use) => {
            Some(ModulePathDefinition::NoDefinition)
        }
        None => {
            if path.cursor_segment + 1 == path.segments.len() {
                if let Some(location) = function_location {
                    return Some(ModulePathDefinition::Location(location));
                }
                return None;
            }
            Some(ModulePathDefinition::NoDefinition)
        }
    }
}

fn function_path_definition(
    snapshot: &marrow_check::AnalysisSnapshot,
    indices: &impl FileIndex,
    file: &Path,
    path: &QualifiedPath,
) -> Option<Location> {
    let current = snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == file)?;
    let Resolution::Found(definition) = resolve(
        &snapshot.program,
        &current.name,
        &path.segments,
        ResolvableKind::Function,
    ) else {
        return None;
    };
    let DefItem::Function(function) = definition.item else {
        return None;
    };
    function_location(
        snapshot,
        indices,
        &definition.module.source_file,
        &function.name,
    )
}

enum ModulePathDefinition {
    Location(Location),
    NoDefinition,
}

struct QualifiedPath {
    segments: Vec<String>,
    cursor_segment: usize,
    declaration: Option<ModulePathDeclaration>,
}

impl QualifiedPath {
    fn binding_index_should_win(&self, kind: SymbolKind) -> bool {
        matches!(kind, SymbolKind::Enum | SymbolKind::EnumMember)
    }
}

#[derive(Clone, Copy)]
enum ModulePathDeclaration {
    Module,
    Use,
}

fn qualified_path_at(
    source: &str,
    offset: usize,
    in_type_annotation: bool,
) -> Option<QualifiedPath> {
    let tokens = lex_source(source).tokens;
    let cursor = tokens
        .iter()
        .position(|token| is_possible_path_segment(token) && span_covers(token.span, offset))?;

    let mut first = cursor;
    while first >= 2
        && tokens[first - 1].kind == TokenKind::DoubleColon
        && is_possible_path_segment(&tokens[first - 2])
    {
        first -= 2;
    }

    let mut last = cursor;
    while last + 2 < tokens.len()
        && tokens[last + 1].kind == TokenKind::DoubleColon
        && is_possible_path_segment(&tokens[last + 2])
    {
        last += 2;
    }

    let mut segment_tokens = Vec::new();
    let mut index = first;
    loop {
        if !is_possible_path_segment(&tokens[index]) {
            return None;
        }
        segment_tokens.push(index);
        if index == last {
            break;
        }
        if index + 2 > last || tokens[index + 1].kind != TokenKind::DoubleColon {
            return None;
        }
        index += 2;
    }

    let cursor_segment = segment_tokens
        .iter()
        .position(|segment| *segment == cursor)?;
    let declaration = path_declaration(&tokens, first);
    if segment_tokens
        .iter()
        .any(|segment| !is_path_segment(&tokens[*segment], declaration, in_type_annotation))
    {
        return None;
    }
    let segments = segment_tokens
        .iter()
        .map(|segment| tokens[*segment].text(source).to_string())
        .collect();
    Some(QualifiedPath {
        segments,
        cursor_segment,
        declaration,
    })
}

fn is_possible_path_segment(token: &Token) -> bool {
    matches!(token.kind, TokenKind::Identifier | TokenKind::Keyword(_))
}

fn is_path_segment(
    token: &Token,
    declaration: Option<ModulePathDeclaration>,
    in_type_annotation: bool,
) -> bool {
    match token.kind {
        TokenKind::Identifier => true,
        TokenKind::Keyword(keyword) => {
            declaration.is_some() || in_type_annotation || is_callable_path_keyword(keyword)
        }
        _ => false,
    }
}

fn is_callable_path_keyword(keyword: Keyword) -> bool {
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
            | Keyword::ErrorCode
            | Keyword::Error
    )
}

fn path_declaration(tokens: &[Token], first_segment: usize) -> Option<ModulePathDeclaration> {
    match tokens.get(first_segment.checked_sub(1)?)?.kind {
        TokenKind::Keyword(Keyword::Module) => Some(ModulePathDeclaration::Module),
        TokenKind::Keyword(Keyword::Use) => Some(ModulePathDeclaration::Use),
        _ => None,
    }
}

fn type_annotation_at(file: &SourceFile, offset: usize) -> bool {
    file.declarations
        .iter()
        .any(|declaration| declaration_type_annotation_at(declaration, offset))
}

fn declaration_type_annotation_at(declaration: &Declaration, offset: usize) -> bool {
    match declaration {
        Declaration::Const(declaration) => declaration
            .ty
            .as_ref()
            .is_some_and(|ty| type_ref_covers(ty, offset)),
        Declaration::Resource(resource) => resource
            .members
            .iter()
            .any(|member| resource_member_type_annotation_at(member, offset)),
        Declaration::Store(store) => store
            .root
            .keys
            .iter()
            .any(|key| type_ref_covers(&key.ty, offset)),
        Declaration::Function(function) => {
            function
                .params
                .iter()
                .any(|param| type_ref_covers(&param.ty, offset))
                || function
                    .return_type
                    .as_ref()
                    .is_some_and(|ty| type_ref_covers(ty, offset))
                || block_type_annotation_at(&function.body, offset)
        }
        Declaration::Evolve(evolve) => evolve
            .steps
            .iter()
            .any(|step| evolve_step_type_annotation_at(step, offset)),
        Declaration::Enum(_) => false,
    }
}

fn evolve_step_type_annotation_at(step: &EvolveStep, offset: usize) -> bool {
    match step {
        EvolveStep::Transform { body, .. } => block_type_annotation_at(body, offset),
        EvolveStep::Rename { .. } | EvolveStep::Default { .. } | EvolveStep::Retire { .. } => false,
    }
}

fn resource_member_type_annotation_at(member: &ResourceMember, offset: usize) -> bool {
    match member {
        ResourceMember::Field(field) => {
            type_ref_covers(&field.ty, offset)
                || field
                    .keys
                    .iter()
                    .any(|key| type_ref_covers(&key.ty, offset))
        }
        ResourceMember::Group(group) => {
            group
                .keys
                .iter()
                .any(|key| type_ref_covers(&key.ty, offset))
                || group
                    .members
                    .iter()
                    .any(|member| resource_member_type_annotation_at(member, offset))
        }
    }
}

fn block_type_annotation_at(block: &marrow_syntax::Block, offset: usize) -> bool {
    block
        .statements
        .iter()
        .any(|statement| statement_type_annotation_at(statement, offset))
}

fn statement_type_annotation_at(statement: &Statement, offset: usize) -> bool {
    match statement {
        Statement::Const { ty, .. } => ty.as_ref().is_some_and(|ty| type_ref_covers(ty, offset)),
        Statement::Var { keys, ty, .. } => {
            keys.iter().any(|key| type_ref_covers(&key.ty, offset))
                || ty.as_ref().is_some_and(|ty| type_ref_covers(ty, offset))
        }
        Statement::If {
            then_block,
            else_ifs,
            else_block,
            ..
        } => {
            block_type_annotation_at(then_block, offset)
                || else_ifs
                    .iter()
                    .any(|else_if| block_type_annotation_at(&else_if.block, offset))
                || else_block
                    .as_ref()
                    .is_some_and(|block| block_type_annotation_at(block, offset))
        }
        Statement::While { body, .. }
        | Statement::For { body, .. }
        | Statement::Transaction { body, .. } => block_type_annotation_at(body, offset),
        Statement::Try {
            body,
            catch,
            finally,
            ..
        } => {
            block_type_annotation_at(body, offset)
                || catch.as_ref().is_some_and(|catch| {
                    catch
                        .ty
                        .as_ref()
                        .is_some_and(|ty| type_ref_covers(ty, offset))
                        || block_type_annotation_at(&catch.block, offset)
                })
                || finally
                    .as_ref()
                    .is_some_and(|block| block_type_annotation_at(block, offset))
        }
        Statement::Match { arms, .. } => arms
            .iter()
            .any(|arm| block_type_annotation_at(&arm.block, offset)),
        Statement::Assign { .. }
        | Statement::Delete { .. }
        | Statement::Return { .. }
        | Statement::Break { .. }
        | Statement::Continue { .. }
        | Statement::Throw { .. }
        | Statement::Expr { .. } => false,
    }
}

fn type_ref_covers(ty: &TypeRef, offset: usize) -> bool {
    span_covers(ty.span, offset)
}

fn function_location(
    snapshot: &marrow_check::AnalysisSnapshot,
    indices: &impl FileIndex,
    file: &Path,
    function_name: &str,
) -> Option<Location> {
    let analyzed = snapshot
        .files
        .iter()
        .find(|analyzed| analyzed.path == file)?;
    for declaration in &analyzed.parsed.file.declarations {
        let Declaration::Function(function) = declaration else {
            continue;
        };
        if function.name != function_name {
            continue;
        }
        let (start, end) = name_in_span(
            &analyzed.source,
            function.span.start_byte,
            function.span.end_byte,
            &function.name,
        )?;
        let line_index = indices.index_for(file)?;
        let url = Url::from_file_path(file).ok()?;
        return Some(Location {
            uri: url,
            range: line_index.range(start, end),
        });
    }
    None
}

/// The source spelling of a symbol's name: the identifier the checker resolved it
/// under, recovered from its span. A *use* of a single-segment name spans exactly
/// the identifier; a declaration span opens with a keyword, and a qualified use is
/// a `::`-separated path — in either case the name is the last identifier run in
/// the span, so scanning for it skips the leading keyword and any module qualifier.
fn symbol_name(symbol: &SymbolRef, indices: &impl FileIndex) -> Option<String> {
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
    let lexed = lex_source(name);
    if !lexed.diagnostics.is_empty() {
        return false;
    }

    let mut tokens = lexed
        .tokens
        .iter()
        .filter(|token| token.kind != TokenKind::Eof);
    let Some(token) = tokens.next() else {
        return false;
    };

    tokens.next().is_none()
        && token.kind == TokenKind::Identifier
        && token.span.start_byte == 0
        && token.span.end_byte == name.len()
}

#[cfg(test)]
mod tests;
