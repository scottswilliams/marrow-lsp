use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::positions::LineIndex;

/// Resolves a file's [`LineIndex`] so navigation can map a span to an LSP range.
///
/// A request maps each referenced file through this: the open buffer's cached
/// index when one is held, else an index built from disk. Returning `None` for a
/// file drops its references from the result rather than failing the whole
/// request.
pub trait FileIndex {
    fn index_for(&self, file: &Path) -> Option<&LineIndex>;
}

/// A resolver over the files an analysis touched, holding one [`LineIndex`] per
/// file: a borrowed index for an open buffer and an owned index built from disk
/// for a file no buffer holds.
pub struct SnapshotIndices<'a> {
    open: HashMap<PathBuf, &'a LineIndex>,
    disk: HashMap<PathBuf, LineIndex>,
}

impl<'a> SnapshotIndices<'a> {
    /// Index every file in `files`: use `open(path)`'s borrowed index when the
    /// file is an open buffer, else build one from the file's disk text.
    pub fn new(
        files: impl IntoIterator<Item = &'a Path>,
        open: impl Fn(&Path) -> Option<&'a LineIndex>,
    ) -> Self {
        let mut borrowed = HashMap::new();
        let mut owned = HashMap::new();
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
