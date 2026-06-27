//! Go-to-definition, find-references, and rename over project analysis.
//!
//! Public navigation is backed by the checker's
//! [`BindingIndex`](marrow_check::BindingIndex) plus canonical catalog spans for
//! saved-data roots, members, and indexes. Imported module aliases and checked
//! qualified leaves come from BindingIndex facts where Marrow emits them;
//! remaining project module prefixes come from Marrow source module-path facts
//! after BindingIndex has declined the cursor.

mod catalog_uses;
mod indices;
mod source_names;
mod symbols;

pub use indices::{FileIndex, SnapshotIndices};
pub use symbols::{RenameError, definition, prepare_rename, references, rename};

#[cfg(test)]
mod tests;
