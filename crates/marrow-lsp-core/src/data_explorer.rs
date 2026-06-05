//! Root-level saved-data presentation for the editor tree view.
//!
//! The LSP keeps saved-data identity opaque until Marrow exposes typed place,
//! child, cursor, and generation facts. This module lists only canonical saved
//! roots through Marrow tooling facts; it does not accept client-authored saved
//! paths.

use serde::{Deserialize, Serialize};

use marrow_check::CheckedProgram;

use crate::store::{Availability, LiveStore};

/// `marrow/savedRoots` reply: the saved root names, plus whether the store could
/// be read. When `available` is false the roots are empty and the tree view shows
/// an unavailable state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRootsResult {
    pub available: bool,
    pub roots: Vec<String>,
}

/// Handle `marrow/savedRoots`. A `None` reader (no native store configured) or an
/// unavailable store both answer `available: false` with no roots.
pub fn saved_roots(reader: Option<&LiveStore>, program: &CheckedProgram) -> SavedRootsResult {
    match reader.map(|reader| reader.roots(program)) {
        Some(Availability::Available(roots)) => SavedRootsResult {
            available: true,
            roots,
        },
        _ => SavedRootsResult {
            available: false,
            roots: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_reader_answers_unavailable() {
        let program = CheckedProgram::default();
        assert_eq!(
            saved_roots(None, &program),
            SavedRootsResult {
                available: false,
                roots: Vec::new()
            }
        );
    }
}
