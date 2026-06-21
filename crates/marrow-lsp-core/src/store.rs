//! Saved-data session admission for editor tooling.

use marrow_check::CheckedProgram;
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
