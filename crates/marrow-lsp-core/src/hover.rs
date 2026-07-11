//! Hover: Markdown presentation over Marrow analysis facts.
//!
//! An editor sends a byte offset (already converted from an LSP position by the
//! document's [`LineIndex`](crate::positions::LineIndex)). The LSP builds or
//! accepts a [`BindingIndex`], asks Marrow tooling for callable, operator,
//! module-path, schema, store-root, saved-place, and type hover facts, then
//! renders the selected fact as Markdown.

use std::path::Path;

use lsp_types::Hover;
use marrow_check::tooling::{source_hover_fact_at, source_symbol_docs_at};
use marrow_check::{AnalysisSnapshot, BindingIndex, build_binding_index};

mod render;

/// The hover for byte `offset` in `file`, or `None` when no known symbol or type
/// covers it.
///
/// The file must be one the snapshot analyzed; a path it does not know (or an
/// offset no expression covers) yields `None`, so the editor shows nothing.
pub fn hover(snapshot: &AnalysisSnapshot, file: &Path, offset: usize) -> Option<Hover> {
    let index = build_binding_index(snapshot);
    hover_with_index(snapshot, &index, file, offset)
}

/// The hover for byte `offset`, using `index` for resolved symbol facts.
pub fn hover_with_index(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<Hover> {
    source_hover_fact_at(snapshot, index, file, offset)
        .map(|fact| render::hover(&snapshot.program, fact))
}

pub fn symbol_docs(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<String> {
    render::join_docs(&source_symbol_docs_at(snapshot, index, file, offset)?.lines)
}

#[cfg(test)]
mod tests;
