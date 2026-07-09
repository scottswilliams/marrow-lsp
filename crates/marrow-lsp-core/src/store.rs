//! Saved-data session admission for editor tooling.

use std::path::PathBuf;

use marrow_check::CheckedProgram;
use marrow_run::{
    DataViewBoundary, ProjectSurfaceReadSession, ProjectSurfaceSnapshot,
    data_view_unavailable_reason_for_config, data_view_watch_targets,
};
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
    pub(crate) fn from_read_session(session: ProjectSurfaceReadSession) -> Self {
        Self { session }
    }

    pub(crate) fn surface_read(&self) -> &ProjectSurfaceReadSession {
        &self.session
    }

    pub(crate) fn data_view_boundary(&self) -> &DataViewBoundary {
        self.session.data_view_boundary()
    }

    pub fn matches_checked_program(&self, program: &CheckedProgram) -> bool {
        self.session.admits_checked_program(program)
    }
}

pub fn open_saved_data_session(project: &Project) -> Result<Option<SavedDataSession>, String> {
    if data_view_unavailable_reason_for_config(&project.root, &project.config)
        .map_err(|error| error.message())?
        .is_some()
    {
        return Ok(None);
    }
    ProjectSurfaceReadSession::open(&project.root)
        .map(|session| Some(SavedDataSession { session }))
        .map_err(|error| error.message())
}

/// Open the checked project surface for saved-data reads without acquiring a store handle,
/// or `None` when the project exposes no inspectable native store. The returned snapshot
/// holds the checked program but no store lock, so it can be cached across reads; a live
/// read handle is acquired per read from [`ProjectSurfaceSnapshot::open_read_only`].
pub(crate) fn open_saved_data_snapshot(project: &Project) -> Option<ProjectSurfaceSnapshot> {
    if data_view_unavailable_reason_for_config(&project.root, &project.config)
        .ok()?
        .is_some()
    {
        return None;
    }
    ProjectSurfaceSnapshot::open(&project.root).ok()
}

pub fn saved_data_watch_targets(
    project: Option<&Project>,
    live_data: bool,
) -> Result<SavedDataWatchTargets, String> {
    let Some(project) = project.filter(|_| live_data) else {
        return Ok(SavedDataWatchTargets { paths: Vec::new() });
    };
    Ok(SavedDataWatchTargets {
        paths: data_view_watch_targets(&project.root, &project.config)
            .map_err(|error| error.message())?
            .into_iter()
            .map(|target| target.path)
            .collect(),
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
