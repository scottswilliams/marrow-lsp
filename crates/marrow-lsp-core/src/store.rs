//! Saved-data session admission for editor tooling.

use std::path::PathBuf;

use marrow_check::CheckedProgram;
use marrow_project::{CATALOG_FILE_NAME, StoreBackend, StoreConfig};
use marrow_run::ProjectSurfaceReadSession;
use serde::{Deserialize, Serialize};

use crate::workspace::Project;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDataWatchTargets {
    pub paths: Vec<PathBuf>,
}

pub struct SavedDataSession {
    session: ProjectSurfaceReadSession,
}

impl SavedDataSession {
    pub(crate) fn surface_read(&self) -> &ProjectSurfaceReadSession {
        &self.session
    }

    pub fn matches_checked_program(&self, program: &CheckedProgram) -> bool {
        let session_program = self.session.program();
        session_program.source_digest() == program.source_digest()
            && read_only_context_matches(session_program, program)
    }
}

fn read_only_context_matches(session_program: &CheckedProgram, program: &CheckedProgram) -> bool {
    if session_program.read_only_context_digest() == program.read_only_context_digest() {
        return true;
    }
    if program.catalog.accepted_digest.is_some()
        || session_program.catalog.accepted_digest.is_none()
        || program.catalog.accepted_epoch != session_program.catalog.accepted_epoch
        || program.catalog.accepted_entries != session_program.catalog.accepted_entries
    {
        return false;
    }
    // A lock-bound workspace snapshot carries the accepted entries and epoch but
    // not the native store's accepted catalog digest. Fill only that missing
    // digest, then use Marrow's own read-only context identity calculation.
    let mut admitted = program.clone();
    admitted.catalog.accepted_digest = session_program.catalog.accepted_digest.clone();
    session_program.read_only_context_digest() == admitted.read_only_context_digest()
}

pub fn open_saved_data_session(project: &Project) -> Result<Option<SavedDataSession>, String> {
    let StoreConfig { backend, data_dir } = &project.config.store;
    match backend {
        StoreBackend::Memory => Ok(None),
        StoreBackend::Native if data_dir.is_none() => Ok(None),
        StoreBackend::Native => ProjectSurfaceReadSession::open(&project.root)
            .map(|session| Some(SavedDataSession { session }))
            .map_err(|error| error.message()),
    }
}

pub fn saved_data_watch_targets(
    project: Option<&Project>,
    live_data: bool,
) -> Result<SavedDataWatchTargets, String> {
    let Some(project) = project.filter(|_| live_data) else {
        return Ok(SavedDataWatchTargets { paths: Vec::new() });
    };
    let Some(store_path) = marrow_check::native_store_path(&project.root, &project.config)
        .map_err(|error| error.message())?
    else {
        return Ok(SavedDataWatchTargets { paths: Vec::new() });
    };
    Ok(SavedDataWatchTargets {
        paths: vec![store_path, project.root.join(CATALOG_FILE_NAME)],
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use marrow_project::{CATALOG_FILE_NAME, parse_config};

    use super::*;

    fn project(root: &Path, config: &str) -> Project {
        Project {
            root: root.to_path_buf(),
            config: parse_config(config).unwrap(),
        }
    }

    #[test]
    fn saved_data_watch_targets_follow_validated_project_store_config() {
        let root = PathBuf::from("/project");
        let native = project(
            &root,
            r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": ".marrow/data" } }"#,
        );

        let targets = saved_data_watch_targets(Some(&native), true).unwrap();
        assert_eq!(
            targets.paths,
            vec![
                root.join(".marrow/data/marrow.redb"),
                root.join(CATALOG_FILE_NAME),
            ]
        );

        let memory = project(
            &root,
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        );
        assert!(
            saved_data_watch_targets(Some(&memory), true)
                .unwrap()
                .paths
                .is_empty(),
            "memory projects should not invent a store-file watch target"
        );
        assert!(
            saved_data_watch_targets(Some(&native), false)
                .unwrap()
                .paths
                .is_empty(),
            "watch targets stay unavailable while live data is disabled"
        );
        assert!(
            saved_data_watch_targets(None, true)
                .unwrap()
                .paths
                .is_empty(),
            "no current project means no saved-data watch target"
        );
    }
}
