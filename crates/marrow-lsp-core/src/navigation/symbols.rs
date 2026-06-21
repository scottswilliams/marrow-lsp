use std::{collections::HashMap, path::Path};

use lsp_types::{Location, Range, TextEdit, Url, WorkspaceEdit};
use marrow_check::tooling::source_module_path_definition_fact_at;
use marrow_check::{BindingIndex, RenameAction, RenameSafety, SymbolRef};

use super::{
    catalog_uses,
    indices::FileIndex,
    saved_roots,
    source_names::{is_valid_rename, name_in_span, name_in_span_at, symbol_name},
};

/// Why a rename could not be carried out, for the caller to turn into a JSON-RPC
/// error or a quiet refusal as its transport requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameError {
    /// The cursor is not on a renameable symbol.
    NoSymbol,
    /// The cursor is on a known symbol, but Marrow has no source-only rename action for it.
    NotRenameable,
    /// The requested replacement is not a single valid Marrow identifier.
    InvalidName,
    /// The symbol names data encoded on disk. Renaming it in source alone would
    /// change the on-disk path and orphan the stored data, so it is refused.
    SavedDataBacked,
}

impl RenameError {
    /// A human-readable explanation, used as the message a transport surfaces.
    pub fn message(&self) -> String {
        match self {
            Self::NoSymbol => "no renameable symbol at this position".to_string(),
            Self::NotRenameable => "this symbol cannot be renamed from the editor".to_string(),
            Self::InvalidName => "new name must be a valid Marrow identifier".to_string(),
            Self::SavedDataBacked => "cannot rename: this names saved data, so renaming it in \
                 source would orphan the stored records on disk"
                .to_string(),
        }
    }
}

/// The definition site of the symbol at byte `offset` in `file`, as an LSP
/// location, or `None` when the cursor is not on a known symbol.
pub fn definition(
    snapshot: &marrow_check::AnalysisSnapshot,
    index: &BindingIndex,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
) -> Option<Location> {
    if let Some(location) = catalog_uses::definition(snapshot, indices, file, offset) {
        return Some(location);
    }
    if saved_roots::root_syntax_at(snapshot, file, offset) {
        return None;
    }
    if let Some(symbol) = index.definition(file, offset) {
        return symbol_location(&symbol, indices);
    }
    source_module_path_definition_location(snapshot, index, indices, file, offset)
}

/// Every reference to the symbol at byte `offset` in `file`, as LSP locations.
pub fn references(
    snapshot: &marrow_check::AnalysisSnapshot,
    index: &BindingIndex,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    if let Some(locations) =
        catalog_uses::references(snapshot, indices, file, offset, include_declaration)
    {
        return Some(locations);
    }
    let definition = index.definition(file, offset)?;
    let refs = index.references(&definition);
    let locations = refs
        .iter()
        .filter(|reference| include_declaration || **reference != definition)
        .filter_map(|reference| symbol_location(reference, indices))
        .collect();
    Some(locations)
}

/// The identifier range the editor should pre-select for a rename at `offset`.
pub fn prepare_rename(
    index: &BindingIndex,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
) -> Option<Range> {
    let definition = index.definition(file, offset)?;
    source_only_rename_action(index, &definition, "rename").ok()?;
    let name = symbol_name(&definition, indices)?;
    let line_index = indices.index_for(file)?;
    let (start, end) = name_in_span_at(line_index.text(), offset, &name)?;
    Some(line_index.range(start, end))
}

/// A workspace edit renaming the symbol at `offset` in `file` to `new_name`.
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
    let action = source_only_rename_action(index, &definition, new_name)?;

    let mut edits: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    for edit in action.edits {
        let Some(line_index) = indices.index_for(&edit.file) else {
            continue;
        };
        let Some(url) = Url::from_file_path(&edit.file).ok() else {
            continue;
        };
        edits.entry(url).or_default().push(TextEdit {
            range: line_index.range(edit.span.start_byte, edit.span.end_byte),
            new_text: edit.replacement,
        });
    }

    Ok(WorkspaceEdit {
        changes: Some(edits),
        document_changes: None,
        change_annotations: None,
    })
}

fn source_only_rename_action(
    index: &BindingIndex,
    definition: &SymbolRef,
    new_name: &str,
) -> Result<RenameAction, RenameError> {
    if let RenameSafety::SavedDataBacked = index.rename_safety(definition) {
        return Err(RenameError::SavedDataBacked);
    }
    let action = index
        .rename_action(definition, new_name)
        .ok_or(RenameError::NotRenameable)?;
    if action.evolve_rename.is_some() {
        return Err(RenameError::SavedDataBacked);
    }
    Ok(action)
}

fn symbol_location(symbol: &SymbolRef, indices: &impl FileIndex) -> Option<Location> {
    let line_index = indices.index_for(&symbol.file)?;
    let url = Url::from_file_path(&symbol.file).ok()?;
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

fn source_module_path_definition_location(
    snapshot: &marrow_check::AnalysisSnapshot,
    index: &BindingIndex,
    indices: &impl FileIndex,
    file: &Path,
    offset: usize,
) -> Option<Location> {
    let fact = source_module_path_definition_fact_at(snapshot, index, file, offset)?;
    let line_index = indices.index_for(&fact.source_file)?;
    let uri = Url::from_file_path(&fact.source_file).ok()?;
    Some(Location {
        uri,
        range: line_index.range(fact.span.start_byte, fact.span.end_byte),
    })
}
