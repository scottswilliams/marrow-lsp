use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::positions::LineIndex;

/// Resolves a file's [`LineIndex`] so navigation can map a span to an LSP range.
///
/// A request maps each referenced file through this: the open buffer's cached
/// index when one is held, else an index built from the snapshot's own source.
/// Returning `None` for a file drops its references from the result rather than
/// failing the whole request.
pub trait FileIndex {
    fn index_for(&self, file: &Path) -> Option<&LineIndex>;
}

/// A resolver over the files an analysis touched, holding one [`LineIndex`] per
/// file: a borrowed index for an open buffer and an owned index built from the
/// snapshot's analyzed source for a file no buffer holds.
pub struct SnapshotIndices<'a> {
    open: HashMap<PathBuf, &'a LineIndex>,
    closed: HashMap<PathBuf, LineIndex>,
}

impl<'a> SnapshotIndices<'a> {
    /// Index every `(path, source)` the analysis touched: use `open(path)`'s
    /// borrowed index when the file is an open buffer, else build one from the
    /// snapshot's own analyzed `source`.
    ///
    /// Building from the snapshot source rather than re-reading disk means the
    /// index is self-consistent with the spans the same snapshot produced — a
    /// closed file that changed on disk since the analysis cannot mislocate a span
    /// — and the navigation read does no filesystem I/O. A file the snapshot did
    /// not analyze is simply absent, so its would-be references are dropped rather
    /// than mapped through an empty index to `0:0`.
    pub fn new(
        files: impl IntoIterator<Item = (&'a Path, &'a str)>,
        open: impl Fn(&Path) -> Option<&'a LineIndex>,
    ) -> Self {
        let mut borrowed = HashMap::new();
        let mut owned = HashMap::new();
        for (file, source) in files {
            if let Some(index) = open(file) {
                borrowed.insert(file.to_path_buf(), index);
            } else {
                owned.insert(file.to_path_buf(), LineIndex::new(source));
            }
        }
        Self {
            open: borrowed,
            closed: owned,
        }
    }
}

impl FileIndex for SnapshotIndices<'_> {
    fn index_for(&self, file: &Path) -> Option<&LineIndex> {
        self.open
            .get(file)
            .copied()
            .or_else(|| self.closed.get(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_a_closed_file_from_source_without_reading_disk() {
        // A path that does not exist on disk: resolving it at all proves the index is built
        // from the snapshot's analyzed source, not from a filesystem read.
        let missing = Path::new("/marrow-lsp-nonexistent/never_created.mw");
        let source = "module m\n\npub fn f(): int\n    return 1\n";
        let indices = SnapshotIndices::new([(missing, source)], |_: &Path| None);

        let index = indices
            .index_for(missing)
            .expect("a closed analyzed file is indexed from its source");
        let offset = source.find("pub fn").expect("source contains the function");
        assert_eq!(
            index.range(offset, offset).start.line,
            2,
            "an offset on the third line maps to line 2, never the 0:0 an empty fallback gives"
        );
        assert_eq!(index.text(), source);
    }

    #[test]
    fn a_file_the_snapshot_did_not_analyze_has_no_index() {
        let known = Path::new("/p/a.mw");
        let indices = SnapshotIndices::new([(known, "module a\n")], |_: &Path| None);
        assert!(
            indices.index_for(Path::new("/p/other.mw")).is_none(),
            "an unanalyzed target yields no location instead of mapping through an empty 0:0 index"
        );
    }
}
