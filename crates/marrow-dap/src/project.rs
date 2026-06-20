//! Load a Marrow project for a debug launch.
//!
//! A launch points at a project directory (the one holding `marrow.json`). We
//! open the canonical Marrow project session with isolated writes and admit the
//! requested or configured entry invocation. The debugger runs through that
//! session so catalog, store, and runtime facts stay owned by Marrow.

use std::path::{Path, PathBuf};

use marrow_check::{AnalysisIdentity, AnalysisSnapshot, CheckReport, ProjectIoError};
use marrow_run::{
    CheckedEntryCall, EntryArgument, EntryDescriptor, EntryDescriptorError, EntryInvocation,
    ProjectOpen, ProjectSession, ProjectSessionError, RuntimeError,
};
use marrow_syntax::Diagnose;

/// The project config file whose directory is the project root.
const CONFIG_FILE: &str = "marrow.json";
const STATUS_BLOCKED_ON_MARROW: &str = "blocked-on-marrow";
const STATUS_INVALID_PARAMS: &str = "invalid-params";
const STATUS_INVALID_PROJECT: &str = "invalid-project";
const STATUS_INVALID_STATE: &str = "invalid-state";
const CATALOG_REPAIR_FACTS: &str = "read-only debug catalog admission facts";

/// A loaded project ready to debug through Marrow's protocol invocation.
pub struct Launch {
    pub session: ProjectSession,
    pub analysis_identity: AnalysisIdentity,
    pub analysis_snapshot: AnalysisSnapshot,
    pub ephemeral_entry_identity: bool,
    pub invocation: EntryInvocation,
}

/// Why a launch could not be prepared. Each renders to a single client-facing
/// message on the failed `launch` response.
#[derive(Debug)]
pub enum LaunchError {
    NoProject(PathBuf),
    Config(String),
    Session(String),
    Entry(String),
    EntryArgs(String),
    EntryChanged,
    AnalysisChanged,
    NoEntry,
    CatalogRepairBlocked,
    CheckErrors(Vec<String>),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoProject(dir) => {
                write!(f, "no {CONFIG_FILE} found at {}", dir.display())
            }
            Self::Config(message) => write!(f, "invalid {CONFIG_FILE}: {message}"),
            Self::Session(message) => write!(f, "could not open the debug session: {message}"),
            Self::Entry(message) => write!(f, "invalid launch entry: {message}"),
            Self::EntryArgs(message) => write!(f, "invalid launch args: {message}"),
            Self::EntryChanged => write!(
                f,
                "launch target changed between launch and configurationDone; launch again"
            ),
            Self::AnalysisChanged => write!(
                f,
                "debug expression analysis changed while preparing launch; launch again"
            ),
            Self::NoEntry => write!(
                f,
                "no entry to run: pass `entry` or configure `defaultEntry` in {CONFIG_FILE}"
            ),
            Self::CatalogRepairBlocked => write!(
                f,
                "blocked-on-marrow: DAP launch will not repair accepted catalog artifacts from the store; use Marrow's production catalog flow or wait for read-only debug catalog admission facts"
            ),
            Self::CheckErrors(errors) => {
                write!(f, "the project has errors: {}", errors.join("; "))
            }
        }
    }
}

impl LaunchError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoProject(_) | Self::Config(_) | Self::Session(_) | Self::CheckErrors(_) => {
                "dap.launchProject.invalid"
            }
            Self::Entry(_) | Self::NoEntry => "dap.launchEntry.invalid",
            Self::EntryArgs(_) => "dap.launchArgs.invalid",
            Self::EntryChanged => "dap.launch.changed",
            Self::AnalysisChanged => "dap.launchAnalysis.changed",
            Self::CatalogRepairBlocked => "dap.launchCatalogRepair.blocked",
        }
    }

    pub fn status(&self) -> &'static str {
        match self {
            Self::CatalogRepairBlocked => STATUS_BLOCKED_ON_MARROW,
            Self::Entry(_) | Self::EntryArgs(_) => STATUS_INVALID_PARAMS,
            Self::EntryChanged | Self::AnalysisChanged => STATUS_INVALID_STATE,
            _ => STATUS_INVALID_PROJECT,
        }
    }

    pub fn blocked_on(&self) -> Option<&'static str> {
        match self {
            Self::CatalogRepairBlocked => Some(CATALOG_REPAIR_FACTS),
            Self::NoProject(_)
            | Self::Config(_)
            | Self::Session(_)
            | Self::Entry(_)
            | Self::EntryArgs(_)
            | Self::EntryChanged
            | Self::AnalysisChanged
            | Self::NoEntry
            | Self::CheckErrors(_) => None,
        }
    }
}

/// Prepare a launch: resolve the project at `project_dir`, open Marrow's session
/// with isolated writes, and admit the requested entry invocation against the
/// current checked program.
pub fn prepare(
    project_dir: &Path,
    entry: Option<&str>,
    args: &[EntryArgument],
) -> Result<Launch, LaunchError> {
    let (session, descriptor) = prepare_descriptor(project_dir, entry)?;
    let analysis_snapshot = prepare_analysis_snapshot(project_dir, &session)?;
    let invocation = EntryInvocation {
        identity: descriptor.identity,
        arguments: args.to_vec(),
    };
    CheckedEntryCall::from_protocol_invocation(session.runtime_program(), &invocation)
        .map_err(|error| LaunchError::EntryArgs(runtime_error_message(error)))?;

    Ok(Launch {
        analysis_identity: session.source_analysis_identity().clone(),
        analysis_snapshot,
        ephemeral_entry_identity: uses_ephemeral_entry_identity(project_dir, &session),
        session,
        invocation,
    })
}

pub fn prepare_admitted(
    project_dir: &Path,
    entry: Option<&str>,
    analysis_identity: &AnalysisIdentity,
    ephemeral_entry_identity: bool,
    invocation: EntryInvocation,
) -> Result<Launch, LaunchError> {
    let (session, descriptor) = prepare_descriptor(project_dir, entry)?;
    if session.source_analysis_identity() != analysis_identity {
        return Err(LaunchError::EntryChanged);
    }
    let analysis_snapshot = prepare_analysis_snapshot(project_dir, &session)?;
    if descriptor.identity.canonical_name != invocation.identity.canonical_name {
        return Err(LaunchError::EntryChanged);
    }
    let current_ephemeral =
        ephemeral_entry_identity && uses_ephemeral_entry_identity(project_dir, &session);
    let invocation = if current_ephemeral {
        EntryInvocation {
            identity: descriptor.identity,
            arguments: invocation.arguments,
        }
    } else {
        invocation
    };
    CheckedEntryCall::from_protocol_invocation(session.runtime_program(), &invocation)
        .map_err(|error| LaunchError::EntryArgs(runtime_error_message(error)))?;

    Ok(Launch {
        analysis_identity: session.source_analysis_identity().clone(),
        analysis_snapshot,
        ephemeral_entry_identity: current_ephemeral,
        session,
        invocation,
    })
}

fn prepare_descriptor(
    project_dir: &Path,
    entry: Option<&str>,
) -> Result<(ProjectSession, EntryDescriptor), LaunchError> {
    let config_path = project_dir.join(CONFIG_FILE);
    if !config_path.is_file() {
        return Err(LaunchError::NoProject(project_dir.to_path_buf()));
    }
    guard_session_open_is_read_only(project_dir)?;
    let mut open = ProjectOpen::run().with_isolated_writes();
    if let Some(entry) = entry {
        open = open.with_entry_override(entry);
    }
    let session = ProjectSession::open(project_dir, open).map_err(from_session_error)?;
    let entry = session
        .run_entry()
        .map(str::to_string)
        .ok_or(LaunchError::NoEntry)?;
    let descriptor = EntryDescriptor::resolve(session.runtime_program(), &entry)
        .map_err(|error| LaunchError::Entry(entry_descriptor_error_message(error).to_string()))?;
    Ok((session, descriptor))
}

fn prepare_analysis_snapshot(
    project_dir: &Path,
    session: &ProjectSession,
) -> Result<AnalysisSnapshot, LaunchError> {
    let accepted =
        marrow_check::read_accepted_catalog_artifact(project_dir).map_err(from_project_io_error)?;
    let snapshot = marrow_check::check_source_project_analysis_against(
        project_dir,
        session.config(),
        accepted.as_ref(),
    )
    .map_err(from_project_io_error)?;
    if snapshot.content_identity() != session.source_analysis_identity() {
        return Err(LaunchError::AnalysisChanged);
    }
    Ok(snapshot)
}

fn uses_ephemeral_entry_identity(project_dir: &Path, session: &ProjectSession) -> bool {
    !project_dir.join(marrow_project::CATALOG_FILE_NAME).exists()
        && matches!(session.store_stamp(), Ok(None))
}

fn guard_session_open_is_read_only(project_dir: &Path) -> Result<(), LaunchError> {
    let config = marrow_check::load_config(project_dir).map_err(from_project_io_error)?;
    let Some(store_path) =
        marrow_check::native_store_path(project_dir, &config).map_err(from_project_io_error)?
    else {
        return Ok(());
    };
    if !store_path.exists() {
        return Ok(());
    }

    match marrow_check::read_accepted_catalog_artifact(project_dir) {
        Ok(Some(_)) => Ok(()),
        Ok(None) | Err(ProjectIoError::Catalog { .. }) => Err(LaunchError::CatalogRepairBlocked),
        Err(ProjectIoError::Io { error, .. })
            if error.kind() == std::io::ErrorKind::InvalidData =>
        {
            Err(LaunchError::CatalogRepairBlocked)
        }
        Err(error) => Err(from_project_io_error(error)),
    }
}

fn from_project_io_error(error: ProjectIoError) -> LaunchError {
    match error {
        ProjectIoError::Io { path, error } => {
            LaunchError::Session(format!("{}: {error}", path.display()))
        }
        ProjectIoError::DataDirCreate { path, error } => {
            LaunchError::Session(format!("{}: {error}", path.display()))
        }
        ProjectIoError::Config { message, .. } => LaunchError::Config(message),
        ProjectIoError::Catalog { message, .. } => LaunchError::Session(message),
        ProjectIoError::Check { report } => LaunchError::CheckErrors(check_errors(&report)),
        ProjectIoError::CheckLoad { path, message, .. } => {
            LaunchError::Session(format!("{}: {message}", path.display()))
        }
        ProjectIoError::Store(error) => LaunchError::Session(error.to_string()),
    }
}

fn from_session_error(error: ProjectSessionError) -> LaunchError {
    match error {
        ProjectSessionError::Config { message, .. } => LaunchError::Config(message),
        ProjectSessionError::Check { report } => LaunchError::CheckErrors(check_errors(&report)),
        ProjectSessionError::NoEntry => LaunchError::NoEntry,
        ProjectSessionError::Io { path, error } => {
            LaunchError::Session(format!("{}: {error}", path.display()))
        }
        ProjectSessionError::CheckLoad { path, message, .. } => {
            LaunchError::Session(format!("{}: {message}", path.display()))
        }
        error => LaunchError::Session(error.message()),
    }
}

fn runtime_error_message(error: RuntimeError) -> String {
    format!("{}: {}", error.code(), error.message())
}

fn entry_descriptor_error_message(error: EntryDescriptorError) -> &'static str {
    match error {
        EntryDescriptorError::Ambiguous => "entry selector is ambiguous",
        EntryDescriptorError::Private => "entry selector resolves to a private function",
        EntryDescriptorError::Missing => "entry selector does not resolve to a public function",
    }
}

fn check_errors(report: &CheckReport) -> Vec<String> {
    report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == marrow_syntax::Severity::Error)
        .map(|diagnostic| diagnostic.message.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_run::{Host, ProjectSessionNotice, SessionEntry};

    fn write_project(source: &str, config: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("m.mw"), source).unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), config).unwrap();
        dir
    }

    fn remove_catalog_after_native_seed(dir: &Path) {
        let session = ProjectSession::open(dir, ProjectOpen::run()).expect("seed session");
        let host = Host::new();
        let mut output = String::new();
        session
            .invoke(SessionEntry::new("m::main", &host, &mut output))
            .expect("seed run");

        let catalog = dir.join(marrow_project::CATALOG_FILE_NAME);
        assert!(catalog.exists(), "seed run should write accepted catalog");
        assert!(
            dir.join("data").join("marrow.redb").exists(),
            "seed run should create the native store"
        );
        std::fs::remove_file(catalog).expect("remove accepted catalog");
    }

    fn corrupt_catalog_utf8_after_native_seed(dir: &Path) {
        let session = ProjectSession::open(dir, ProjectOpen::run()).expect("seed session");
        let host = Host::new();
        let mut output = String::new();
        session
            .invoke(SessionEntry::new("m::main", &host, &mut output))
            .expect("seed run");

        let catalog = dir.join(marrow_project::CATALOG_FILE_NAME);
        assert!(catalog.exists(), "seed run should write accepted catalog");
        std::fs::write(catalog, [0xff]).expect("write invalid UTF-8 catalog");
    }

    #[test]
    fn prepare_resolves_the_default_entry_with_a_memory_store() {
        let dir = write_project(
            "module m\n\npub fn main(): int\n    return 1\n",
            "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"m::main\" }, \"store\": { \"backend\": \"memory\" } }",
        );
        let launch = prepare(dir.path(), None, &[]).unwrap();
        assert_eq!(launch.invocation.identity.canonical_name, "m::main");
    }

    #[test]
    fn prepare_uses_isolated_session_without_creating_native_store() {
        let dir = write_project(
            "\
module m

resource Book
    required title: string

store ^books(id: int): Book

pub fn main(): int
    return 1
",
            "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"m::main\" }, \"store\": { \"backend\": \"native\", \"dataDir\": \"data\" } }",
        );
        let launch = prepare(dir.path(), None, &[]).unwrap();
        assert_eq!(launch.invocation.identity.canonical_name, "m::main");
        assert_eq!(launch.session.run_entry(), Some("m::main"));
        assert!(
            launch
                .session
                .notices()
                .contains(&ProjectSessionNotice::DryRunWouldFreeze),
            "isolated session should record that a committing run would freeze accepted identity"
        );
        assert!(
            !dir.path().join("marrow.catalog.json").exists(),
            "a presentation-only debug prepare must not write the accepted catalog"
        );
        assert!(
            !dir.path().join("data").join("marrow.redb").exists(),
            "a presentation-only debug prepare must not open the native store"
        );
    }

    #[test]
    fn prepare_blocks_native_store_catalog_repair_without_writing() {
        let dir = write_project(
            "\
module m

resource Book
    required title: string

store ^books(id: int): Book

pub fn main()
    ^books(1).title = \"Dune\"
",
            "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"m::main\" }, \"store\": { \"backend\": \"native\", \"dataDir\": \"data\" } }",
        );
        remove_catalog_after_native_seed(dir.path());

        let error = expect_error(prepare(dir.path(), None, &[]));
        assert_eq!(error.code(), "dap.launchCatalogRepair.blocked");
        assert_eq!(error.status(), "blocked-on-marrow");
        assert_eq!(
            error.blocked_on(),
            Some("read-only debug catalog admission facts")
        );
        assert!(
            !dir.path().join(marrow_project::CATALOG_FILE_NAME).exists(),
            "DAP prepare must not repair the accepted catalog artifact"
        );
    }

    #[test]
    fn prepare_blocks_invalid_utf8_catalog_repair_without_writing() {
        let dir = write_project(
            "\
module m

resource Book
    required title: string

store ^books(id: int): Book

pub fn main()
    ^books(1).title = \"Dune\"
",
            "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"m::main\" }, \"store\": { \"backend\": \"native\", \"dataDir\": \"data\" } }",
        );
        corrupt_catalog_utf8_after_native_seed(dir.path());

        let error = expect_error(prepare(dir.path(), None, &[]));
        assert_eq!(error.code(), "dap.launchCatalogRepair.blocked");
        assert_eq!(error.status(), "blocked-on-marrow");
        assert_eq!(
            error.blocked_on(),
            Some("read-only debug catalog admission facts")
        );
        assert_eq!(
            std::fs::read(dir.path().join(marrow_project::CATALOG_FILE_NAME)).unwrap(),
            [0xff],
            "DAP prepare must not repair the invalid catalog artifact"
        );
    }

    #[test]
    fn prepare_accepts_an_explicit_entry_with_a_descriptor_invocation() {
        let dir = write_project(
            "module m\n\npub fn main(): int\n    return 1\n\npub fn other(): int\n    return 2\n",
            "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"m::main\" }, \"store\": { \"backend\": \"memory\" } }",
        );
        let launch = prepare(dir.path(), Some("m::other"), &[]).unwrap();
        assert_eq!(launch.invocation.identity.requested_name, "m::other");
        assert_eq!(launch.invocation.identity.canonical_name, "m::other");
        assert!(launch.invocation.arguments.is_empty());
    }

    /// The launch error, asserting the call did not unexpectedly succeed without
    /// requiring `Launch` to be `Debug` (its store backends are not).
    fn expect_error(result: Result<Launch, LaunchError>) -> LaunchError {
        match result {
            Ok(_) => panic!("expected a launch error"),
            Err(error) => error,
        }
    }

    #[test]
    fn a_project_with_check_errors_will_not_launch() {
        let dir = write_project(
            "module m\n\npub fn main(): int\n    return \"nope\"\n",
            "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"m::main\" }, \"store\": { \"backend\": \"memory\" } }",
        );
        let error = expect_error(prepare(dir.path(), None, &[]));
        assert!(matches!(error, LaunchError::CheckErrors(_)), "{error}");
    }

    #[test]
    fn missing_entry_error_reports_invalid_launch_entry() {
        let dir = write_project(
            "module m\n\npub fn main(): int\n    return 1\n",
            "{ \"sourceRoots\": [\"src\"], \"store\": { \"backend\": \"memory\" } }",
        );
        let error = expect_error(prepare(dir.path(), None, &[]));
        assert!(matches!(error, LaunchError::NoEntry), "{error}");
        assert_eq!(error.code(), "dap.launchEntry.invalid");
        assert_eq!(error.status(), "invalid-project");
        assert_eq!(error.blocked_on(), None);
    }

    #[test]
    fn a_directory_without_a_config_is_no_project() {
        let dir = tempfile::tempdir().unwrap();
        let error = expect_error(prepare(dir.path(), None, &[]));
        assert!(matches!(error, LaunchError::NoProject(_)), "{error}");
        assert_eq!(error.code(), "dap.launchProject.invalid");
        assert_eq!(error.status(), "invalid-project");
        assert_eq!(error.blocked_on(), None);
    }

    #[test]
    fn data_dir_create_error_reports_the_store_directory() {
        let error = from_project_io_error(ProjectIoError::DataDirCreate {
            path: PathBuf::from("data"),
            error: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        });

        assert!(matches!(error, LaunchError::Session(_)), "{error}");
        assert_eq!(error.code(), "dap.launchProject.invalid");
        assert_eq!(
            error.to_string(),
            "could not open the debug session: data: denied"
        );
    }
}
