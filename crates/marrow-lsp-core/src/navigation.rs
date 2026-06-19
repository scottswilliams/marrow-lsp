//! Go-to-definition, find-references, and rename over project analysis.
//!
//! Public navigation is backed by the checker's
//! [`BindingIndex`](marrow_check::BindingIndex) plus canonical catalog spans for
//! saved-data roots, members, and indexes. Module and use path segments stay
//! unavailable until Marrow exposes canonical module-path facts, except for the
//! existing checked function leaf lookup through `resolve`.

mod catalog_uses;
mod indices;
mod module_paths;
mod source_names;
mod symbols;

pub use indices::{FileIndex, SnapshotIndices};
pub use symbols::{RenameError, definition, prepare_rename, references, rename};

#[cfg(test)]
mod tests;
