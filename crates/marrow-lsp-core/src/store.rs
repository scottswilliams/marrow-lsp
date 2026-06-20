//! Saved-data session admission for editor tooling.

use marrow_project::{StoreBackend, StoreConfig};
use marrow_run::ProjectSurfaceReadSession;

use crate::workspace::Project;

pub struct SavedDataSession {
    session: ProjectSurfaceReadSession,
}

impl SavedDataSession {
    pub(crate) fn surface_read(&self) -> &ProjectSurfaceReadSession {
        &self.session
    }
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
