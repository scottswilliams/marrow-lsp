use marrow_check::CheckedProgram;

pub fn requires_accepted_catalog_identity(program: &CheckedProgram) -> bool {
    program.catalog.accepted_epoch.is_none()
        && program
            .catalog
            .proposal
            .as_ref()
            .is_some_and(|proposal| !proposal.entries.is_empty())
}
