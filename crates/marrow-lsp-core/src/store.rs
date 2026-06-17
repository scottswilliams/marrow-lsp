//! Read-only access to a project's durable tree-cell store for editor tooling.

use std::path::PathBuf;

use marrow_check::CheckedProgram;
use marrow_check::tooling;
use marrow_project::{StoreBackend, StoreConfig};
use marrow_store::StoreError;
use marrow_store::tree::TreeStore;

use crate::data_explorer::{DataChildrenError, DataChildrenRequest, DataChildrenResult};
use crate::workspace::Project;

/// The file name of a native store inside its data directory.
const STORE_FILE: &str = "marrow.redb";

/// A path to a project's native store. Each operation opens the store read-only
/// and delegates data semantics to Marrow's typed tooling APIs.
pub struct LiveStore {
    path: PathBuf,
}

impl LiveStore {
    /// The live store for `project`, or `None` when the project has no native
    /// on-disk store to inspect.
    pub fn for_project(project: &Project) -> Option<Self> {
        let StoreConfig { backend, data_dir } = &project.config.store;
        match backend {
            StoreBackend::Native => {
                let dir = data_dir.as_deref()?;
                Some(Self {
                    path: project.root.join(dir).join(STORE_FILE),
                })
            }
            StoreBackend::Memory => None,
        }
    }

    pub fn roots(&self, program: &CheckedProgram) -> Availability<Vec<String>> {
        self.with_tree_result(|store| tooling::data_roots_in_store(program, store))
    }

    pub fn data_children_page(
        &self,
        program: &CheckedProgram,
        request: DataChildrenRequest,
    ) -> Availability<Result<DataChildrenResult, DataChildrenError>> {
        self.with_tree(|store| crate::data_explorer::data_children_page(program, store, request))
    }

    fn with_tree<T>(&self, read: impl FnOnce(&TreeStore) -> T) -> Availability<T> {
        let Ok(store) = TreeStore::open_read_only(&self.path) else {
            return Availability::Unavailable;
        };
        Availability::Available(read(&store))
    }

    pub(crate) fn with_tree_result<T>(
        &self,
        read: impl FnOnce(&TreeStore) -> Result<T, StoreError>,
    ) -> Availability<T> {
        let Ok(store) = TreeStore::open_read_only(&self.path) else {
            return Availability::Unavailable;
        };
        read(&store)
            .map(Availability::Available)
            .unwrap_or(Availability::Unavailable)
    }
}

/// Whether an inspection could read the store. A store that exists but is locked,
/// corrupt, or of an unsupported format reads as `Unavailable` rather than
/// raising an error, so a transient condition never breaks an editor feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability<T> {
    Available(T),
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_project::parse_config;

    #[test]
    fn a_memory_backend_has_no_reader() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let config_text = r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#;
        let config = parse_config(config_text).unwrap();
        let project = Project { root, config };
        assert!(LiveStore::for_project(&project).is_none());
    }
}
