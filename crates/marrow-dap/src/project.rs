//! Load a Marrow project for a debug launch.
//!
//! A launch points at a project directory (the one holding `marrow.json`). We
//! open the canonical Marrow project session with isolated writes and admit the
//! requested or configured entry invocation. The debugger runs through that
//! session so catalog, store, and runtime facts stay owned by Marrow.

use std::path::{Path, PathBuf};

use marrow_check::{AnalysisGeneration, AnalysisSnapshot, CheckReport, ProjectIoError};
use marrow_run::{
    CheckedEntryCall, EntryArgument, EntryDescriptor, EntryDescriptorError, EntryInvocation,
    ProjectOpen, ProjectSession, ProjectSessionError, ProjectSurfaceReadSession, RuntimeError,
    SourceAnalysisAdmission, SurfaceServeBoundary,
};
use marrow_syntax::Diagnose;

/// The project config file whose directory is the project root.
const CONFIG_FILE: &str = "marrow.json";
const STATUS_INVALID_PARAMS: &str = "invalid-params";
const STATUS_INVALID_PROJECT: &str = "invalid-project";
const STATUS_INVALID_STATE: &str = "invalid-state";

/// A loaded project ready to debug through Marrow's protocol invocation.
pub struct Launch {
    pub session: ProjectSession,
    pub analysis_generation: AnalysisGeneration,
    pub analysis_snapshot: AnalysisSnapshot,
    pub source_analysis_admission: Option<SourceAnalysisAdmission>,
    pub invocation: EntryInvocation,
}

impl Launch {
    pub fn surface_serve_boundary(&self) -> Result<SurfaceServeBoundary, LaunchError> {
        self.session
            .surface_serve_boundary()
            .map_err(from_session_error)
    }
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
    AnalysisChanged(String),
    NoEntry,
    ReadAdmissionInvalid,
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
            Self::AnalysisChanged(message) => write!(f, "{message}; launch again"),
            Self::NoEntry => write!(
                f,
                "no entry to run: pass `entry` or configure `defaultEntry` in {CONFIG_FILE}"
            ),
            Self::ReadAdmissionInvalid => write!(
                f,
                "DAP launch cannot admit the committed store for read-only debug; use Marrow's production data flow before launching"
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
            Self::AnalysisChanged(_) => "dap.launchAnalysis.changed",
            Self::ReadAdmissionInvalid => "dap.launchReadAdmission.invalid",
        }
    }

    pub fn status(&self) -> &'static str {
        match self {
            Self::Entry(_) | Self::EntryArgs(_) => STATUS_INVALID_PARAMS,
            Self::EntryChanged | Self::AnalysisChanged(_) => STATUS_INVALID_STATE,
            _ => STATUS_INVALID_PROJECT,
        }
    }

    pub fn blocked_on(&self) -> Option<&'static str> {
        None
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
    let (session, descriptor) = prepare_descriptor(project_dir, entry, None)?;
    let analysis_snapshot = session.source_analysis_snapshot().clone();
    let analysis_generation = analysis_snapshot.generation();
    let source_analysis_admission = session
        .source_analysis_admission()
        .map_err(from_session_error)?;
    let invocation = EntryInvocation {
        identity: descriptor.identity,
        arguments: args.to_vec(),
    };
    CheckedEntryCall::from_protocol_invocation(session.runtime_program(), &invocation)
        .map_err(|error| LaunchError::EntryArgs(runtime_error_message(error)))?;

    Ok(Launch {
        analysis_generation,
        analysis_snapshot,
        source_analysis_admission,
        session,
        invocation,
    })
}

pub fn prepare_admitted(
    project_dir: &Path,
    entry: Option<&str>,
    analysis_generation: &AnalysisGeneration,
    source_analysis_admission: Option<&SourceAnalysisAdmission>,
    invocation: EntryInvocation,
) -> Result<Launch, LaunchError> {
    let (session, descriptor) = prepare_descriptor(project_dir, entry, source_analysis_admission)?;
    if session.source_analysis_snapshot().generation() != *analysis_generation {
        return Err(LaunchError::AnalysisChanged(
            "source analysis changed between launch and configurationDone".to_string(),
        ));
    }
    let analysis_snapshot = session.source_analysis_snapshot().clone();
    let analysis_generation = analysis_snapshot.generation();
    let current_admission = session
        .source_analysis_admission()
        .map_err(from_session_error)?;
    if descriptor.identity.canonical_name != invocation.identity.canonical_name {
        return Err(LaunchError::EntryChanged);
    }
    CheckedEntryCall::from_protocol_invocation(session.runtime_program(), &invocation)
        .map_err(|error| LaunchError::EntryArgs(runtime_error_message(error)))?;

    Ok(Launch {
        analysis_generation,
        analysis_snapshot,
        source_analysis_admission: current_admission,
        session,
        invocation,
    })
}

fn prepare_descriptor(
    project_dir: &Path,
    entry: Option<&str>,
    source_analysis_admission: Option<&SourceAnalysisAdmission>,
) -> Result<(ProjectSession, EntryDescriptor), LaunchError> {
    let config_path = project_dir.join(CONFIG_FILE);
    if !config_path.is_file() {
        match marrow_check::load_config(project_dir) {
            Err(ProjectIoError::ConfigMissing { .. }) => {
                return Err(LaunchError::NoProject(project_dir.to_path_buf()));
            }
            Err(error) => return Err(from_project_io_error(error)),
            Ok(_) => {}
        }
    }
    guard_session_open_is_read_only(project_dir)?;
    let mut open = ProjectOpen::run().with_isolated_writes();
    if let Some(entry) = entry {
        open = open.with_entry_override(entry);
    }
    if let Some(admission) = source_analysis_admission {
        open = open.with_source_analysis_admission(admission.clone());
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

    ProjectSurfaceReadSession::open(project_dir)
        .map(|_| ())
        .map_err(from_read_only_session_error)
}

fn from_project_io_error(error: ProjectIoError) -> LaunchError {
    match error {
        ProjectIoError::Io { path, error } => {
            LaunchError::Session(format!("{}: {error}", path.display()))
        }
        error @ ProjectIoError::DataDirCreate { .. } => LaunchError::Config(error.message()),
        ProjectIoError::ConfigMissing { dir } => LaunchError::Config(format!(
            "no {CONFIG_FILE} in {}; run `marrow init {}` or open a directory containing {CONFIG_FILE}",
            dir.display(),
            dir.display()
        )),
        error @ ProjectIoError::NotAProject { .. } => LaunchError::Config(error.message()),
        ProjectIoError::Config { message, .. } => LaunchError::Config(message),
        ProjectIoError::Catalog { message, .. } => LaunchError::Session(message),
        ProjectIoError::Check { report } => LaunchError::CheckErrors(check_errors(&report)),
        ProjectIoError::CheckLoad { path, message, .. } => {
            LaunchError::Session(format!("{}: {message}", path.display()))
        }
        ProjectIoError::Store(error) => LaunchError::Session(error.to_string()),
    }
}

fn from_read_only_session_error(error: ProjectSessionError) -> LaunchError {
    match error {
        ProjectSessionError::Catalog { .. }
        | ProjectSessionError::CheckLoad { .. }
        | ProjectSessionError::DurableStoreRequired
        | ProjectSessionError::UnstampedStore
        | ProjectSessionError::Store(_)
        | ProjectSessionError::SchemaDrift { .. } => LaunchError::ReadAdmissionInvalid,
        ProjectSessionError::Io { error, .. }
            if error.kind() == std::io::ErrorKind::InvalidData =>
        {
            LaunchError::ReadAdmissionInvalid
        }
        error => from_session_error(error),
    }
}

fn from_session_error(error: ProjectSessionError) -> LaunchError {
    match error {
        ProjectSessionError::Config { message, .. } => LaunchError::Config(message),
        ProjectSessionError::Catalog { .. } => LaunchError::ReadAdmissionInvalid,
        ProjectSessionError::Check { report } => LaunchError::CheckErrors(check_errors(&report)),
        ProjectSessionError::NoEntry => LaunchError::NoEntry,
        ProjectSessionError::Io { path, error } => {
            LaunchError::Session(format!("{}: {error}", path.display()))
        }
        ProjectSessionError::CheckLoad { path, message, .. } => {
            LaunchError::Session(format!("{}: {message}", path.display()))
        }
        ProjectSessionError::SchemaDrift { message } => LaunchError::AnalysisChanged(message),
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
    use marrow_store::tree::TreeStore;

    fn write_project(source: &str, config: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("m.mw"), source).unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), config).unwrap();
        dir
    }

    fn seed_native_store(dir: &Path) {
        let session = ProjectSession::open(dir, ProjectOpen::run()).expect("seed session");
        let host = Host::new();
        let mut output = String::new();
        session
            .invoke(SessionEntry::new("m::main", &host, &mut output))
            .expect("seed run");

        let catalog = dir.join(marrow_project::CATALOG_FILE_NAME);
        assert!(catalog.exists(), "seed run should write committed lock");
        assert!(
            dir.join("data").join("marrow.redb").exists(),
            "seed run should create the native store"
        );
    }

    fn corrupt_catalog_utf8_with_empty_native_store(dir: &Path) {
        let data_dir = dir.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        drop(TreeStore::open(&data_dir.join("marrow.redb")).unwrap());
        let catalog = dir.join(marrow_project::CATALOG_FILE_NAME);
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
            !dir.path().join(marrow_project::CATALOG_FILE_NAME).exists(),
            "isolated debug prepare must not write the committed lock"
        );
        assert!(
            !dir.path().join("data").join("marrow.redb").exists(),
            "isolated debug prepare must not open the native store"
        );
    }

    #[test]
    fn prepare_accepts_valid_existing_native_store() {
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
        seed_native_store(dir.path());

        let launch = prepare(dir.path(), None, &[]).expect("valid native store is admitted");
        assert_eq!(launch.invocation.identity.canonical_name, "m::main");
    }

    #[test]
    fn prepare_rejects_invalid_utf8_read_admission_without_writing() {
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
        corrupt_catalog_utf8_with_empty_native_store(dir.path());

        let error = expect_error(prepare(dir.path(), None, &[]));
        assert_eq!(error.code(), "dap.launchReadAdmission.invalid", "{error:?}");
        assert_eq!(error.status(), "invalid-project");
        assert_eq!(error.blocked_on(), None);
        assert_eq!(
            std::fs::read(dir.path().join(marrow_project::CATALOG_FILE_NAME)).unwrap(),
            [0xff],
            "DAP prepare must not mutate the invalid committed lock"
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
    fn a_bare_file_project_path_is_not_a_project() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.mw");
        std::fs::write(&path, "module demo\n").unwrap();
        let project_error = ProjectIoError::NotAProject { path: path.clone() };
        let expected = format!("invalid {CONFIG_FILE}: {}", project_error.message());

        let error = expect_error(prepare(&path, None, &[]));

        assert!(matches!(error, LaunchError::Config(_)), "{error}");
        assert_eq!(error.code(), "dap.launchProject.invalid");
        assert_eq!(error.status(), "invalid-project");
        assert_eq!(error.blocked_on(), None);
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn a_bare_file_project_path_with_a_trailing_separator_is_not_a_project() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.mw");
        std::fs::write(&path, "module demo\n").unwrap();
        let project = PathBuf::from(format!("{}/", path.display()));
        let project_error = ProjectIoError::NotAProject {
            path: project.clone(),
        };
        let expected = format!("invalid {CONFIG_FILE}: {}", project_error.message());

        let error = expect_error(prepare(&project, None, &[]));

        assert!(matches!(error, LaunchError::Config(_)), "{error}");
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn a_project_path_under_a_file_component_is_not_a_project() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.mw");
        std::fs::write(&path, "module demo\n").unwrap();
        let project = path.join("nested");
        let project_error = ProjectIoError::NotAProject {
            path: project.clone(),
        };
        let expected = format!("invalid {CONFIG_FILE}: {}", project_error.message());

        let error = expect_error(prepare(&project, None, &[]));

        assert!(matches!(error, LaunchError::Config(_)), "{error}");
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn an_unreadable_config_entry_is_not_reported_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(CONFIG_FILE)).unwrap();
        let project_error = marrow_check::load_config(dir.path()).unwrap_err();
        let expected = from_project_io_error(project_error).to_string();

        let error = expect_error(prepare(dir.path(), None, &[]));

        assert!(matches!(error, LaunchError::Config(_)), "{error}");
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn data_dir_create_error_reports_the_store_directory() {
        let project_error = ProjectIoError::DataDirCreate {
            path: PathBuf::from("data"),
            fault: marrow_check::DataDirFault::PermissionDenied,
        };
        let expected = format!("invalid {CONFIG_FILE}: {}", project_error.message());
        let error = from_project_io_error(project_error);

        assert!(matches!(error, LaunchError::Config(_)), "{error}");
        assert_eq!(error.code(), "dap.launchProject.invalid");
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn config_missing_error_names_the_directory_and_remedy() {
        let error = from_project_io_error(ProjectIoError::ConfigMissing {
            dir: PathBuf::from("/projects/demo"),
        });

        assert!(matches!(error, LaunchError::Config(_)), "{error}");
        assert_eq!(error.code(), "dap.launchProject.invalid");
        let message = error.to_string();
        assert!(message.contains("/projects/demo"), "{message}");
        assert!(message.contains("marrow init"), "{message}");
    }

    #[test]
    fn not_a_project_error_names_the_bare_file() {
        let project_error = ProjectIoError::NotAProject {
            path: PathBuf::from("/projects/demo.mw"),
        };
        let expected = format!("invalid {CONFIG_FILE}: {}", project_error.message());
        let error = from_project_io_error(project_error);

        assert!(matches!(error, LaunchError::Config(_)), "{error}");
        assert_eq!(error.code(), "dap.launchProject.invalid");
        assert_eq!(error.to_string(), expected);
    }
}
