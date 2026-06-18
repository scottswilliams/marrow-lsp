//! Hover: presentation over checked facts, as a `marrow` code block.
//!
//! An editor sends a byte offset (already converted from an LSP position by the
//! document's [`LineIndex`](crate::positions::LineIndex)). Positive semantic hovers
//! render checker facts such as binding-index symbols and expression types. Local
//! token and parse inspection only decides presentation targets or suppresses
//! surfaces whose canonical Marrow facts are not exposed yet.

use std::path::Path;

use lsp_types::Hover;
use marrow_check::{AnalysisSnapshot, BindingIndex, build_binding_index};

mod facts;
mod render;
mod source;
mod tokens;

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
    facts::collect(snapshot, index, file, offset).map(render::hover)
}

pub fn symbol_docs(
    snapshot: &AnalysisSnapshot,
    index: &BindingIndex,
    file: &Path,
    offset: usize,
) -> Option<String> {
    render::join_docs(facts::symbol_docs_lines(snapshot, index, file, offset)?)
}

#[cfg(test)]
fn hover_with_docs(
    snapshot: &AnalysisSnapshot,
    file: &Path,
    offset: usize,
    docs: Option<&str>,
) -> Option<Hover> {
    let ty = facts::checked_type_at(snapshot, file, offset)?;
    Some(render::type_hover_with_docs_text(&ty, docs))
}

#[cfg(test)]
mod tests;
