//! Go-to-definition, find-references, and rename over project analysis.
//!
//! Most navigation reads the checker's [`BindingIndex`](marrow_check::BindingIndex)
//! — the one resolution layer that maps every identifier use to the definition it
//! names, respecting lexical scope, `use` aliases, and the saved-data schema.
//! Go-to-definition also has small syntax-backed fallbacks for saved roots and
//! module path prefixes, since those name durable data or namespaces rather than
//! checker symbols. References and rename stay binding-index driven, and rename
//! applies the index's safety classification so a rename that would orphan stored
//! data is refused rather than performed.
//!
//! Spans into a file are mapped through that file's [`LineIndex`]. A caller supplies
//! the index per file (the open buffer's when present, else one built from disk),
//! mirroring how diagnostics resolve a file's position mapping.

use std::path::Path;

use lsp_types::{Location, Range, TextEdit, Url, WorkspaceEdit};
use marrow_check::{
    BindingIndex, DefItem, RenameSafety, Resolution, ResolvableKind, SymbolKind, SymbolRef,
    build_alias_map, expand_module_alias, resolve,
};
use marrow_schema::stdlib;
use marrow_syntax::{
    Declaration, Keyword, ResourceMember, SourceFile, SourceSpan, Statement, Token, TokenKind,
    TypeRef, lex_source,
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
    /// The symbol names data encoded on disk (a saved field, group, layer, index,
    /// or a resource attached to a saved root). Renaming it in source alone would
    /// change the on-disk path and orphan the stored data, so it is refused.
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
    if let Some(location) = saved_root_definition(snapshot, indices, file, offset) {
        return Some(location);
    }
    match module_path_definition(snapshot, index, indices, file, offset) {
        Some(ModulePathDefinition::Location(location)) => return Some(location),
        Some(ModulePathDefinition::NoDefinition) => return None,
        None => {}
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
    if path.declaration.is_none()
        && in_type_annotation
        && let Some(location) = resource_identity_path_definition(snapshot, indices, file, &path)
    {
        return Some(ModulePathDefinition::Location(location));
    }
    let module_name = match path.declaration {
        Some(ModulePathDeclaration::Module | ModulePathDeclaration::Use) => {
            path.segments[..=path.cursor_segment].join("::")
        }
        None => {
            if path.cursor_segment + 1 == path.segments.len() {
                if let Some(location) = function_location {
                    return Some(ModulePathDefinition::Location(location));
                }
                return None;
            }
            let prefix = path.segments[..=path.cursor_segment].join("::");
            let current = snapshot
                .program
                .modules
                .iter()
                .find(|module| module.source_file == file)?;
            let aliases = build_alias_map(&current.imports);
            expand_module_alias(&prefix, &aliases)
        }
    };
    if is_stdlib_module_name(&module_name) {
        return Some(ModulePathDefinition::NoDefinition);
    }
    Some(
        match module_declaration_location(snapshot, indices, &module_name) {
            Some(location) => ModulePathDefinition::Location(location),
            None => ModulePathDefinition::NoDefinition,
        },
    )
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
        matches!(
            kind,
            SymbolKind::Enum | SymbolKind::EnumMember | SymbolKind::ResourceIdentity
        )
    }
}

fn is_stdlib_module_name(name: &str) -> bool {
    name.strip_prefix("std::")
        .is_some_and(|module| stdlib::all().iter().any(|op| op.module == module))
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
        Declaration::Resource(resource) => {
            resource
                .members
                .iter()
                .any(|member| resource_member_type_annotation_at(member, offset))
                || resource.store.as_ref().is_some_and(|store| {
                    store
                        .keys
                        .iter()
                        .any(|key| type_ref_covers(&key.ty, offset))
                })
        }
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
        Declaration::Enum(_) => false,
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
        ResourceMember::Index(_) => false,
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
        | Statement::Transaction { body, .. }
        | Statement::Lock { body, .. } => block_type_annotation_at(body, offset),
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
        | Statement::Merge { .. }
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

fn module_declaration_location(
    snapshot: &marrow_check::AnalysisSnapshot,
    indices: &impl FileIndex,
    module_name: &str,
) -> Option<Location> {
    let module = snapshot
        .program
        .modules
        .iter()
        .find(|module| module.name == module_name)?;
    let line_index = indices.index_for(&module.source_file)?;
    let url = Url::from_file_path(&module.source_file).ok()?;
    let leaf = module.name.rsplit("::").next().unwrap_or(&module.name);
    let range = last_name_in_span(
        line_index.text(),
        module.span.start_byte,
        module.span.end_byte,
        leaf,
    )
    .map(|(start, end)| line_index.range(start, end))
    .unwrap_or_else(|| line_index.range(module.span.start_byte, module.span.end_byte));
    Some(Location { uri: url, range })
}

fn resource_identity_path_definition(
    snapshot: &marrow_check::AnalysisSnapshot,
    indices: &impl FileIndex,
    file: &Path,
    path: &QualifiedPath,
) -> Option<Location> {
    let (id, prefix) = path.segments.split_last()?;
    if id != "Id" || path.cursor_segment + 2 < path.segments.len() {
        return None;
    }
    let current = snapshot
        .program
        .modules
        .iter()
        .find(|module| module.source_file == file)?;
    let (resource_name, module_segments) = prefix.split_last()?;
    if module_segments.is_empty() {
        let Resolution::Found(definition) = resolve(
            &snapshot.program,
            &current.name,
            &path.segments,
            ResolvableKind::ResourceIdentity,
        ) else {
            return None;
        };
        let DefItem::Resource(resource) = definition.item else {
            return None;
        };
        return resource_identity_location(
            snapshot,
            indices,
            &definition.module.source_file,
            &resource.name,
        );
    }

    let aliases = build_alias_map(&current.imports);
    let module_name = expand_module_alias(&module_segments.join("::"), &aliases);
    let module = snapshot
        .program
        .modules
        .iter()
        .find(|module| module.name == module_name)?;
    let resource = module
        .resources
        .iter()
        .find(|resource| resource.name.as_str() == resource_name.as_str())?;
    resource_identity_location(snapshot, indices, &module.source_file, &resource.name)
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

fn resource_identity_location(
    snapshot: &marrow_check::AnalysisSnapshot,
    indices: &impl FileIndex,
    file: &Path,
    resource_name: &str,
) -> Option<Location> {
    let analyzed = snapshot
        .files
        .iter()
        .find(|analyzed| analyzed.path == file)?;
    for declaration in &analyzed.parsed.file.declarations {
        let Declaration::Resource(resource) = declaration else {
            continue;
        };
        if resource.name != resource_name {
            continue;
        }
        let (start, end) = resource_identity_span(&analyzed.source, resource)?;
        let line_index = indices.index_for(file)?;
        let url = Url::from_file_path(file).ok()?;
        return Some(Location {
            uri: url,
            range: line_index.range(start, end),
        });
    }
    None
}

fn resource_identity_span(
    source: &str,
    resource: &marrow_syntax::ResourceDecl,
) -> Option<(usize, usize)> {
    if let Some(store) = &resource.store
        && let Some(span) = saved_root_identifier_span(source, resource.span, &store.root)
    {
        return Some(span);
    }
    name_in_span(
        source,
        resource.span.start_byte,
        resource.span.end_byte,
        &resource.name,
    )
}

fn last_name_in_span(text: &str, start: usize, end: usize, name: &str) -> Option<(usize, usize)> {
    let end = end.min(text.len());
    if start > end {
        return None;
    }
    let slice = text.get(start..end)?;
    let mut search_from = 0;
    let mut found_range = None;
    while let Some(found) = slice[search_from..].find(name) {
        let abs_start = start + search_from + found;
        let abs_end = abs_start + name.len();
        if is_whole_word(text, abs_start, abs_end) {
            found_range = Some((abs_start, abs_end));
        }
        search_from += found + 1;
        if search_from >= slice.len() {
            break;
        }
    }
    found_range
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
            matches!(result, Err(RenameError::SavedDataBacked)),
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
    fn module_path_prefix_definition_from_imported_call_jumps_to_module_declaration() {
        let books_source = "\
module shelf::books

pub fn titleOf(): string
    return \"Dune\"
";
        let app_source = "\
module shelf::app

use shelf::books

pub fn run(): string
    return books::titleOf()
";
        let (snapshot, paths, indices) = analyze_files(&[
            ("shelf/books.mw", books_source),
            ("shelf/app.mw", app_source),
        ]);
        let books_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let prefix = offset_of(app_source, "return books::titleOf") + "return ".len();
        let location = definition(&snapshot, &index, &indices, app_file, prefix + 1)
            .expect("module prefix resolves to the imported module declaration");
        let line_index = indices.0.get(books_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(books_file).unwrap());
        assert_eq!(
            range_text(books_source, line_index, location.range),
            "books"
        );
        let module_name = offset_of(books_source, "module shelf::books") + "module shelf::".len();
        let (line, character) = line_col(books_source, module_name);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn module_path_leaf_definition_from_imported_call_stays_on_binding_index() {
        let books_source = "\
module shelf::books

pub fn titleOf(): string
    return \"Dune\"
";
        let app_source = "\
module shelf::app

use shelf::books

pub fn run(): string
    return books::titleOf()
";
        let (snapshot, paths, indices) = analyze_files(&[
            ("shelf/books.mw", books_source),
            ("shelf/app.mw", app_source),
        ]);
        let books_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let leaf = offset_of(app_source, "books::titleOf") + "books::".len();
        let location = definition(&snapshot, &index, &indices, app_file, leaf + 1)
            .expect("leaf call still resolves through the binding index");
        let line_index = indices.0.get(books_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(books_file).unwrap());
        assert_eq!(
            range_text(books_source, line_index, location.range),
            "titleOf"
        );
        let function_name = offset_of(books_source, "fn titleOf") + "fn ".len();
        let (line, character) = line_col(books_source, function_name);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn definition_from_qualified_resource_constructor_leaf_jumps_to_foreign_resource_root() {
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
        let (snapshot, paths, indices) = analyze_files(&[
            ("shelf/state.mw", state_source),
            ("shelf/app.mw", app_source),
        ]);
        let state_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let book = offset_of(app_source, "state::Book(title") + "state::".len();
        let location = definition(&snapshot, &index, &indices, app_file, book + 1)
            .expect("qualified resource constructor leaf resolves to the foreign resource");
        let state_index = indices.0.get(state_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(state_file).unwrap());
        assert_eq!(
            range_text(state_source, state_index, location.range),
            "state_books"
        );
    }

    #[test]
    fn definition_from_qualified_resource_identity_constructor_jumps_to_foreign_identity_root() {
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

pub fn make_id()
    const id = state::Book::Id(1)
";
        let (snapshot, paths, indices) = analyze_files(&[
            ("shelf/state.mw", state_source),
            ("shelf/app.mw", app_source),
        ]);
        let state_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);
        let state_index = indices.0.get(state_file).unwrap();

        for offset in [
            offset_of(app_source, "state::Book::Id(1)") + "state::".len(),
            offset_of(app_source, "state::Book::Id(1)") + "state::Book::".len(),
        ] {
            let location = definition(&snapshot, &index, &indices, app_file, offset + 1)
                .expect("qualified resource identity constructor resolves to foreign identity");
            assert_eq!(location.uri, Url::from_file_path(state_file).unwrap());
            assert_eq!(
                range_text(state_source, state_index, location.range),
                "state_books"
            );
        }
    }

    #[test]
    fn definition_from_same_named_local_and_foreign_resource_uses_the_qualified_target() {
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

pub fn make_foreign()
    const book = state::Book(title: \"x\")

pub fn make_local()
    const book = Book(label: \"local\")
";
        let (snapshot, paths, indices) = analyze_files(&[
            ("shelf/state.mw", state_source),
            ("shelf/app.mw", app_source),
        ]);
        let state_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);
        let state_index = indices.0.get(state_file).unwrap();
        let app_index = indices.0.get(app_file).unwrap();

        let qualified_book = offset_of(app_source, "state::Book(title") + "state::".len();
        let qualified_location =
            definition(&snapshot, &index, &indices, app_file, qualified_book + 1)
                .expect("qualified constructor resolves to the imported resource");
        assert_eq!(
            qualified_location.uri,
            Url::from_file_path(state_file).unwrap()
        );
        assert_eq!(
            range_text(state_source, state_index, qualified_location.range),
            "state_books"
        );

        let bare_book = offset_of(app_source, "Book(label: \"local\")");
        let bare_location = definition(&snapshot, &index, &indices, app_file, bare_book + 1)
            .expect("bare constructor resolves to the local resource");
        assert_eq!(bare_location.uri, Url::from_file_path(app_file).unwrap());
        assert_eq!(
            range_text(app_source, app_index, bare_location.range),
            "app_books"
        );
    }

    #[test]
    fn definition_from_same_named_local_and_foreign_identity_uses_the_qualified_target() {
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

pub fn foreign_id()
    const id = state::Book::Id(1)

pub fn local_id()
    const id = Book::Id(\"local\")
";
        let (snapshot, paths, indices) = analyze_files(&[
            ("shelf/state.mw", state_source),
            ("shelf/app.mw", app_source),
        ]);
        let state_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);
        let state_index = indices.0.get(state_file).unwrap();
        let app_index = indices.0.get(app_file).unwrap();

        for offset in [
            offset_of(app_source, "state::Book::Id(1)") + "state::".len(),
            offset_of(app_source, "state::Book::Id(1)") + "state::Book::".len(),
        ] {
            let location = definition(&snapshot, &index, &indices, app_file, offset + 1)
                .expect("qualified identity resolves to the imported resource");
            assert_eq!(location.uri, Url::from_file_path(state_file).unwrap());
            assert_eq!(
                range_text(state_source, state_index, location.range),
                "state_books"
            );
        }

        for offset in [
            offset_of(app_source, "Book::Id(\"local\")"),
            offset_of(app_source, "Book::Id(\"local\")") + "Book::".len(),
        ] {
            let location = definition(&snapshot, &index, &indices, app_file, offset + 1)
                .expect("bare identity resolves to the local resource");
            assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
            assert_eq!(
                range_text(app_source, app_index, location.range),
                "app_books"
            );
        }
    }

    #[test]
    fn module_path_definition_from_use_leaf_jumps_to_imported_module_declaration() {
        let books_source = "\
module shelf::books

pub fn titleOf(): string
    return \"Dune\"
";
        let app_source = "\
module shelf::app

use shelf::books

pub fn run(): string
    return books::titleOf()
";
        let (snapshot, paths, indices) = analyze_files(&[
            ("shelf/books.mw", books_source),
            ("shelf/app.mw", app_source),
        ]);
        let books_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let imported_leaf = offset_of(app_source, "use shelf::books") + "use shelf::".len();
        let location = definition(&snapshot, &index, &indices, app_file, imported_leaf + 1)
            .expect("use leaf resolves to the imported module declaration");
        let line_index = indices.0.get(books_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(books_file).unwrap());
        assert_eq!(
            range_text(books_source, line_index, location.range),
            "books"
        );
        let module_name = offset_of(books_source, "module shelf::books") + "module shelf::".len();
        let (line, character) = line_col(books_source, module_name);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn module_path_prefix_definition_for_std_path_returns_none() {
        let clock_source = "\
module std::clock

const TICK: int = 1
";
        let app_source = "\
module shelf::app

use shelf::clock

pub fn run(): instant
    return std::clock::now()
";
        let (snapshot, paths, indices) =
            analyze_files(&[("std/clock.mw", clock_source), ("shelf/app.mw", app_source)]);
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let std_clock = offset_of(app_source, "std::clock::now") + "std::".len();
        let location = definition(&snapshot, &index, &indices, app_file, std_clock + 1);

        assert_eq!(
            location, None,
            "std module prefixes must not fabricate a project location"
        );
    }

    #[test]
    fn module_path_prefix_definition_for_aliased_std_path_returns_none() {
        let clock_source = "\
module std::clock

const TICK: int = 1
";
        let app_source = "\
module shelf::app

use std::clock

pub fn run(): instant
    return clock::now()
";
        let (snapshot, paths, indices) =
            analyze_files(&[("std/clock.mw", clock_source), ("shelf/app.mw", app_source)]);
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let std_clock = offset_of(app_source, "clock::now");
        let location = definition(&snapshot, &index, &indices, app_file, std_clock + 1);

        assert_eq!(
            location, None,
            "aliased std module prefixes must not fabricate a project location"
        );
    }

    #[test]
    fn module_path_prefix_definition_allows_project_std_namespace_module() {
        let custom_source = "\
module std::custom

pub fn tick(): int
    return 1
";
        let app_source = "\
module app

pub fn run(): int
    return std::custom::tick()
";
        let (snapshot, paths, indices) =
            analyze_files(&[("std/custom.mw", custom_source), ("app.mw", app_source)]);
        let custom_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let custom = offset_of(app_source, "std::custom::tick") + "std::".len();
        let location = definition(&snapshot, &index, &indices, app_file, custom + 1)
            .expect("non-stdlib std:: project module prefix should resolve");
        let line_index = indices.0.get(custom_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(custom_file).unwrap());
        assert_eq!(
            range_text(custom_source, line_index, location.range),
            "custom"
        );
    }

    #[test]
    fn module_path_prefix_definition_does_not_steal_enum_literal_head() {
        let status_module_source = "\
module status

pub fn helper(): int
    return 1
";
        let app_source = "\
module app

enum status
    active

pub fn run(): status
    return status::active
";
        let (snapshot, paths, indices) =
            analyze_files(&[("status.mw", status_module_source), ("app.mw", app_source)]);
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let enum_head = offset_of(app_source, "return status::active") + "return ".len();
        let location = definition(&snapshot, &index, &indices, app_file, enum_head + 1)
            .expect("enum literal head resolves through the binding index");
        let line_index = indices.0.get(app_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
        assert_eq!(range_text(app_source, line_index, location.range), "status");
        let enum_name = offset_of(app_source, "enum status") + "enum ".len();
        let (line, character) = line_col(app_source, enum_name);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn module_path_prefix_definition_does_not_steal_nested_enum_literal_head() {
        let cat_module_source = "\
module cat

pub fn helper(): int
    return 1
";
        let app_source = "\
module app

enum cat
    category tiger
        paw

pub fn run(): cat
    return cat::tiger::paw
";
        let (snapshot, paths, indices) =
            analyze_files(&[("cat.mw", cat_module_source), ("app.mw", app_source)]);
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let enum_head = offset_of(app_source, "return cat::tiger::paw") + "return ".len();
        let location = definition(&snapshot, &index, &indices, app_file, enum_head + 1)
            .expect("nested enum literal head resolves through the binding index");
        let line_index = indices.0.get(app_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
        assert_eq!(range_text(app_source, line_index, location.range), "cat");
        let enum_name = offset_of(app_source, "enum cat") + "enum ".len();
        let (line, character) = line_col(app_source, enum_name);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn module_path_prefix_definition_does_not_steal_nested_enum_member_segment() {
        let tiger_module_source = "\
module cat::tiger

pub fn helper(): int
    return 1
";
        let app_source = "\
module app

enum cat
    category tiger
        paw

pub fn run(): cat
    return cat::tiger::paw
";
        let (snapshot, paths, indices) = analyze_files(&[
            ("cat/tiger.mw", tiger_module_source),
            ("app.mw", app_source),
        ]);
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let member_segment = offset_of(app_source, "return cat::tiger::paw") + "return cat::".len();
        let location = definition(&snapshot, &index, &indices, app_file, member_segment + 1)
            .expect("nested enum member segment resolves through the binding index");
        let line_index = indices.0.get(app_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
        assert_eq!(range_text(app_source, line_index, location.range), "tiger");
        let member_name = offset_of(app_source, "category tiger") + "category ".len();
        let (line, character) = line_col(app_source, member_name);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn module_path_prefix_definition_does_not_steal_resource_identity_head() {
        let book_module_source = "\
module book

pub fn helper(): int
    return 1
";
        let app_source = "\
module app

resource book at ^books(id: int)
    required title: string

pub fn run(): book::Id
    return book::Id(1)
";
        let (snapshot, paths, indices) =
            analyze_files(&[("book.mw", book_module_source), ("app.mw", app_source)]);
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let identity_head = offset_of(app_source, "return book::Id") + "return ".len();
        let location = definition(&snapshot, &index, &indices, app_file, identity_head + 1)
            .expect("resource identity head resolves through the binding index");
        let line_index = indices.0.get(app_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
        assert_eq!(range_text(app_source, line_index, location.range), "books");
        let saved_root =
            offset_of(app_source, "resource book at ^books") + "resource book at ^".len();
        let (line, character) = line_col(app_source, saved_root);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn module_path_prefix_definition_does_not_steal_resource_identity_annotation_head() {
        let book_module_source = "\
module book

pub fn helper(): int
    return 1
";
        let app_source = "\
module app

resource book at ^books(id: int)
    required title: string

pub fn run(): book::Id
    return book::Id(1)
";
        let (snapshot, paths, indices) =
            analyze_files(&[("book.mw", book_module_source), ("app.mw", app_source)]);
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let identity_head =
            offset_of(app_source, "pub fn run(): book::Id") + "pub fn run(): ".len();
        let location = definition(&snapshot, &index, &indices, app_file, identity_head + 1)
            .expect("identity annotation should resolve as the resource identity");
        let line_index = indices.0.get(app_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
        assert_eq!(range_text(app_source, line_index, location.range), "books");
        let saved_root =
            offset_of(app_source, "resource book at ^books") + "resource book at ^".len();
        let (line, character) = line_col(app_source, saved_root);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn module_path_prefix_definition_does_not_steal_imported_module_named_like_identity() {
        let book_source = "\
module shelf::book

pub fn Id(): int
    return 1
";
        let app_source = "\
module app

use shelf::book

resource book at ^books(id: int)
    required title: string

pub fn run(): int
    return book::Id()
";
        let (snapshot, paths, indices) =
            analyze_files(&[("shelf/book.mw", book_source), ("app.mw", app_source)]);
        let book_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);
        let book_index = indices.0.get(book_file).unwrap();

        let prefix = offset_of(app_source, "return book::Id") + "return ".len();
        let prefix_location = definition(&snapshot, &index, &indices, app_file, prefix + 1)
            .expect("aliased module prefix resolves before local resource identity lookup");
        assert_eq!(prefix_location.uri, Url::from_file_path(book_file).unwrap());
        assert_eq!(
            range_text(book_source, book_index, prefix_location.range),
            "book"
        );

        let leaf = offset_of(app_source, "return book::Id") + "return book::".len();
        let leaf_location = definition(&snapshot, &index, &indices, app_file, leaf + 1)
            .expect("aliased leaf resolves to the imported function");
        assert_eq!(leaf_location.uri, Url::from_file_path(book_file).unwrap());
        assert_eq!(
            range_text(book_source, book_index, leaf_location.range),
            "Id"
        );
    }

    #[test]
    fn module_path_definition_keeps_imported_function_without_identity_binding_fact() {
        let book_source = "\
module shelf::book

pub fn Id(): int
    return 1
";
        let app_source = "\
module app

use shelf::book

pub fn run(): int
    return book::Id()
";
        let (snapshot, paths, indices) =
            analyze_files(&[("shelf/book.mw", book_source), ("app.mw", app_source)]);
        let book_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);
        let book_index = indices.0.get(book_file).unwrap();

        let prefix = offset_of(app_source, "return book::Id") + "return ".len();
        let prefix_location = definition(&snapshot, &index, &indices, app_file, prefix + 1)
            .expect("module prefix resolves to the imported module");
        assert_eq!(prefix_location.uri, Url::from_file_path(book_file).unwrap());
        assert_eq!(
            range_text(book_source, book_index, prefix_location.range),
            "book"
        );

        let leaf = offset_of(app_source, "book::Id") + "book::".len();
        let leaf_location = definition(&snapshot, &index, &indices, app_file, leaf + 1)
            .expect("leaf call resolves to the imported function");
        assert_eq!(leaf_location.uri, Url::from_file_path(book_file).unwrap());
        assert_eq!(
            range_text(book_source, book_index, leaf_location.range),
            "Id"
        );
    }

    #[test]
    fn module_path_prefix_definition_named_argument_colon_is_not_type_annotation() {
        let book_source = "\
module shelf::book

pub fn Id(): int
    return 1
";
        let app_source = "\
module app

use shelf::book

resource book at ^books(id: int)
    required title: string

pub fn wrap(value: int): int
    return value

pub fn run(): int
    return wrap(value: book::Id())
";
        let (snapshot, paths, indices) =
            analyze_files(&[("shelf/book.mw", book_source), ("app.mw", app_source)]);
        let book_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);
        let book_index = indices.0.get(book_file).unwrap();

        let prefix =
            offset_of(app_source, "return wrap(value: book::Id") + "return wrap(value: ".len();
        let prefix_location = definition(&snapshot, &index, &indices, app_file, prefix + 1)
            .expect("named argument value stays in expression context for aliased calls");
        assert_eq!(prefix_location.uri, Url::from_file_path(book_file).unwrap());
        assert_eq!(
            range_text(book_source, book_index, prefix_location.range),
            "book"
        );

        let leaf = offset_of(app_source, "return wrap(value: book::Id")
            + "return wrap(value: book::".len();
        let leaf_location = definition(&snapshot, &index, &indices, app_file, leaf + 1)
            .expect("named argument aliased call leaf resolves to the imported function");
        assert_eq!(leaf_location.uri, Url::from_file_path(book_file).unwrap());
        assert_eq!(
            range_text(book_source, book_index, leaf_location.range),
            "Id"
        );
    }

    #[test]
    fn module_path_prefix_definition_resource_identity_inside_sequence_annotation() {
        let book_module_source = "\
module book

pub fn helper(): int
    return 1
";
        let app_source = "\
module app

resource book at ^books(id: int)
    required title: string

pub fn run(ids: sequence[book::Id])
    return
";
        let (snapshot, paths, indices) =
            analyze_files(&[("book.mw", book_module_source), ("app.mw", app_source)]);
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let identity_head = offset_of(app_source, "sequence[book::Id]") + "sequence[".len();
        let location = definition(&snapshot, &index, &indices, app_file, identity_head + 1)
            .expect("identity inside sequence annotation resolves as the resource identity");
        let line_index = indices.0.get(app_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
        assert_eq!(range_text(app_source, line_index, location.range), "books");
        let saved_root =
            offset_of(app_source, "resource book at ^books") + "resource book at ^".len();
        let (line, character) = line_col(app_source, saved_root);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn module_path_prefix_definition_resource_identity_inside_var_key_annotation() {
        let book_module_source = "\
module book

pub fn helper(): int
    return 1
";
        let app_source = "\
module app

resource book at ^books(id: int)
    required title: string

pub fn run()
    var selected(book: book::Id)
";
        let (snapshot, paths, indices) =
            analyze_files(&[("book.mw", book_module_source), ("app.mw", app_source)]);
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let identity_head = offset_of(app_source, "book: book::Id") + "book: ".len();
        let location = definition(&snapshot, &index, &indices, app_file, identity_head + 1)
            .expect("identity in var key annotation resolves as the resource identity");
        let line_index = indices.0.get(app_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
        assert_eq!(range_text(app_source, line_index, location.range), "books");
    }

    #[test]
    fn module_path_prefix_definition_resource_identity_inside_match_arm_block_annotation() {
        let book_module_source = "\
module book

pub fn helper(): int
    return 1
";
        let app_source = "\
module app

resource book at ^books(id: int)
    required title: string

enum status
    active

pub fn run(status: status)
    match status
        active
            var id: book::Id
";
        let (snapshot, paths, indices) =
            analyze_files(&[("book.mw", book_module_source), ("app.mw", app_source)]);
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let identity_head = offset_of(app_source, "var id: book::Id") + "var id: ".len();
        let location = definition(&snapshot, &index, &indices, app_file, identity_head + 1)
            .expect("identity in match arm block annotation resolves as the resource identity");
        let line_index = indices.0.get(app_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(app_file).unwrap());
        assert_eq!(range_text(app_source, line_index, location.range), "books");
    }

    #[test]
    fn module_path_prefix_definition_qualified_resource_identity_annotation() {
        let book_source = "\
module shelf::lib

resource Book at ^books(id: int)
    required title: string
";
        let app_source = "\
module app

use shelf::lib

pub fn read_full(id: shelf::lib::Book::Id): string
    return ^books(id).title

pub fn read_alias(id: lib::Book::Id): string
    return ^books(id).title
";
        let (snapshot, paths, indices) =
            analyze_files(&[("shelf/lib.mw", book_source), ("app.mw", app_source)]);
        let book_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);
        let line_index = indices.0.get(book_file).unwrap();

        for needle in ["shelf::lib::Book::Id", "lib::Book::Id"] {
            let book = offset_of(app_source, needle) + needle.rfind("Book").unwrap();
            let book_location = definition(&snapshot, &index, &indices, app_file, book + 1)
                .expect("qualified identity resource name resolves as the resource identity");
            assert_eq!(book_location.uri, Url::from_file_path(book_file).unwrap());
            assert_eq!(
                range_text(book_source, line_index, book_location.range),
                "books"
            );

            let id = offset_of(app_source, needle) + needle.rfind("Id").unwrap();
            let id_location = definition(&snapshot, &index, &indices, app_file, id + 1)
                .expect("qualified identity leaf resolves as the resource identity");
            assert_eq!(id_location.uri, Url::from_file_path(book_file).unwrap());
            assert_eq!(
                range_text(book_source, line_index, id_location.range),
                "books"
            );
        }
    }

    #[test]
    fn module_path_prefix_definition_resource_identity_annotation_with_keyword_module_segment() {
        let book_source = "\
module unknown::lib

resource Book at ^books(id: int)
    required title: string
";
        let app_source = "\
module app

pub fn read(id: unknown::lib::Book::Id): string
    return ^books(id).title
";
        let (snapshot, paths, indices) =
            analyze_files(&[("unknown/lib.mw", book_source), ("app.mw", app_source)]);
        let book_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);
        let line_index = indices.0.get(book_file).unwrap();

        let module_leaf = offset_of(app_source, "unknown::lib::Book::Id") + "unknown::".len();
        let module_location = definition(&snapshot, &index, &indices, app_file, module_leaf + 1)
            .expect("keyword-like module segment in a type annotation resolves to the module");
        assert_eq!(module_location.uri, Url::from_file_path(book_file).unwrap());
        assert_eq!(
            range_text(book_source, line_index, module_location.range),
            "lib"
        );

        let book = offset_of(app_source, "unknown::lib::Book::Id") + "unknown::lib::".len();
        let book_location = definition(&snapshot, &index, &indices, app_file, book + 1)
            .expect("resource name after a keyword-like module segment resolves as identity");
        assert_eq!(book_location.uri, Url::from_file_path(book_file).unwrap());
        assert_eq!(
            range_text(book_source, line_index, book_location.range),
            "books"
        );

        let id = offset_of(app_source, "unknown::lib::Book::Id") + "unknown::lib::Book::".len();
        let id_location = definition(&snapshot, &index, &indices, app_file, id + 1)
            .expect("identity leaf after a keyword-like module segment resolves as identity");
        assert_eq!(id_location.uri, Url::from_file_path(book_file).unwrap());
        assert_eq!(
            range_text(book_source, line_index, id_location.range),
            "books"
        );
    }

    #[test]
    fn module_path_prefix_definition_repeated_leaf_selects_module_leaf_segment() {
        let foo_source = "\
module foo::foo

pub fn title(): string
    return \"nested\"
";
        let app_source = "\
module app

pub fn run(): string
    return foo::foo::title()
";
        let (snapshot, paths, indices) =
            analyze_files(&[("foo/foo.mw", foo_source), ("app.mw", app_source)]);
        let foo_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let second_prefix = offset_of(app_source, "foo::foo::title") + "foo::".len();
        let location = definition(&snapshot, &index, &indices, app_file, second_prefix + 1)
            .expect("repeated leaf module prefix resolves to the module declaration");
        let line_index = indices.0.get(foo_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(foo_file).unwrap());
        assert_eq!(range_text(foo_source, line_index, location.range), "foo");
        let module_leaf = offset_of(foo_source, "module foo::foo") + "module foo::".len();
        let (line, character) = line_col(foo_source, module_leaf);
        assert_eq!(location.range.start.line, line);
        assert_eq!(location.range.start.character, character);
    }

    #[test]
    fn module_path_namespace_only_prefix_in_fully_qualified_call_returns_none() {
        let books_source = "\
module shelf::books

pub fn titleOf(): string
    return \"Dune\"
";
        let app_source = "\
module shelf::app

pub fn run(): string
    return shelf::books::titleOf()
";
        let (snapshot, paths, indices) = analyze_files(&[
            ("shelf/books.mw", books_source),
            ("shelf/app.mw", app_source),
        ]);
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let namespace = offset_of(app_source, "return shelf::books::titleOf") + "return ".len();
        let location = definition(&snapshot, &index, &indices, app_file, namespace + 1);

        assert_eq!(
            location, None,
            "namespace-only prefixes should not fall through to the call leaf"
        );
    }

    #[test]
    fn module_path_prefix_definition_accepts_keyword_like_segments() {
        let bytes_source = "\
module shelf::bytes

pub fn size(): int
    return 1
";
        let app_source = "\
module shelf::app

use shelf::bytes

pub fn run(): int
    return bytes::size()
";
        let (snapshot, paths, indices) = analyze_files(&[
            ("shelf/bytes.mw", bytes_source),
            ("shelf/app.mw", app_source),
        ]);
        let bytes_file = &paths[0];
        let app_file = &paths[1];
        let index = build_binding_index(&snapshot);

        let prefix = offset_of(app_source, "return bytes::size") + "return ".len();
        let location = definition(&snapshot, &index, &indices, app_file, prefix + 1)
            .expect("keyword-like module prefix resolves to the module declaration");
        let line_index = indices.0.get(bytes_file).unwrap();

        assert_eq!(location.uri, Url::from_file_path(bytes_file).unwrap());
        assert_eq!(
            range_text(bytes_source, line_index, location.range),
            "bytes"
        );
        let module_name = offset_of(bytes_source, "module shelf::bytes") + "module shelf::".len();
        let (line, character) = line_col(bytes_source, module_name);
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
