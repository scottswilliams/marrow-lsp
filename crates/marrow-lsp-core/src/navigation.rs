//! Go-to-definition, find-references, and rename over project analysis.
//!
//! Public navigation stays backed by the checker's
//! [`BindingIndex`](marrow_check::BindingIndex). Module and use path segments stay
//! unavailable until Marrow exposes canonical module-path facts, except for the
//! existing checked function leaf lookup through `resolve`.

mod indices;
mod module_paths;
mod source_names;
mod symbols;

pub use indices::{FileIndex, SnapshotIndices};
pub use symbols::{RenameError, definition, prepare_rename, references, rename};

#[cfg(test)]
mod tests;
