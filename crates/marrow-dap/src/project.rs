//! Load a Marrow project for a debug launch.
//!
//! A launch points at a project directory (the one holding `marrow.json`). We
//! open the canonical Marrow project session with isolated writes and pick the
//! configured default entry. The debugger runs through that session so catalog,
//! store, and runtime facts stay owned by Marrow.

use std::path::{Path, PathBuf};

use marrow_check::{CheckReport, ProjectIoError};
use marrow_run::{ProjectOpen, ProjectSession, ProjectSessionError};

/// The project config file whose directory is the project root.
const CONFIG_FILE: &str = "marrow.json";
const STATUS_BLOCKED_ON_MARROW: &str = "blocked-on-marrow";
const STATUS_INVALID_PROJECT: &str = "invalid-project";
const FUNCTION_ENTRY_FACTS: &str = "canonical function-entry facts";
const CATALOG_REPAIR_FACTS: &str = "read-only debug catalog admission facts";

/// A loaded project ready to debug: the Marrow-owned session and resolved entry.
pub struct Launch {
    pub session: ProjectSession,
    pub entry: String,
}

/// Why a launch could not be prepared. Each renders to a single client-facing
/// message on the failed `launch` response.
#[derive(Debug)]
pub enum LaunchError {
    NoProject(PathBuf),
    Config(String),
    Session(String),
    NoEntry,
    ExplicitEntryBlocked,
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
            Self::NoEntry => write!(
                f,
                "no entry to run: configure `defaultEntry` in {CONFIG_FILE}; canonical function-entry discovery facts are pending in Marrow"
            ),
            Self::ExplicitEntryBlocked => write!(
                f,
                "blocked-on-marrow: explicit DAP entry strings need canonical function-entry facts from Marrow; configure `defaultEntry` in {CONFIG_FILE}"
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
            Self::NoEntry | Self::ExplicitEntryBlocked => "dap.launchEntry.blocked",
            Self::CatalogRepairBlocked => "dap.launchCatalogRepair.blocked",
        }
    }

    pub fn status(&self) -> &'static str {
        if self.blocked_on().is_some() {
            STATUS_BLOCKED_ON_MARROW
        } else {
            STATUS_INVALID_PROJECT
        }
    }

    pub fn blocked_on(&self) -> Option<&'static str> {
        match self {
            Self::NoEntry | Self::ExplicitEntryBlocked => Some(FUNCTION_ENTRY_FACTS),
            Self::CatalogRepairBlocked => Some(CATALOG_REPAIR_FACTS),
            Self::NoProject(_) | Self::Config(_) | Self::Session(_) | Self::CheckErrors(_) => None,
        }
    }
}

/// Prepare a launch: resolve the project at `project_dir`, open Marrow's
/// configured-entry session with isolated writes, and refuse explicit DAP entry
/// strings until Marrow exposes canonical function-entry facts.
pub fn prepare(project_dir: &Path, entry: Option<&str>) -> Result<Launch, LaunchError> {
    if entry.is_some() {
        return Err(LaunchError::ExplicitEntryBlocked);
    }
    let config_path = project_dir.join(CONFIG_FILE);
    if !config_path.is_file() {
        return Err(LaunchError::NoProject(project_dir.to_path_buf()));
    }
    guard_session_open_is_read_only(project_dir)?;
    let session = ProjectSession::open(project_dir, ProjectOpen::run().with_isolated_writes())
        .map_err(from_session_error)?;
    let entry = session
        .run_entry()
        .map(str::to_string)
        .ok_or(LaunchError::NoEntry)?;

    Ok(Launch { session, entry })
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
        let launch = prepare(dir.path(), None).unwrap();
        assert_eq!(launch.entry, "m::main");
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
        let launch = prepare(dir.path(), None).unwrap();
        assert_eq!(launch.entry, "m::main");
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

        let error = expect_error(prepare(dir.path(), None));
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

        let error = expect_error(prepare(dir.path(), None));
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
    fn an_explicit_entry_is_blocked_until_marrow_exposes_entry_facts() {
        let dir = write_project(
            "module m\n\npub fn main(): int\n    return 1\n\npub fn other(): int\n    return 2\n",
            "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"m::main\" }, \"store\": { \"backend\": \"memory\" } }",
        );
        let error = expect_error(prepare(dir.path(), Some("m::other")));
        assert!(
            matches!(error, LaunchError::ExplicitEntryBlocked),
            "{error}"
        );
        assert_eq!(error.code(), "dap.launchEntry.blocked");
        assert_eq!(error.status(), "blocked-on-marrow");
        assert_eq!(error.blocked_on(), Some("canonical function-entry facts"));
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
        let error = expect_error(prepare(dir.path(), None));
        assert!(matches!(error, LaunchError::CheckErrors(_)), "{error}");
    }

    #[test]
    fn missing_entry_error_frames_the_blocked_entry_contract() {
        let dir = write_project(
            "module m\n\npub fn main(): int\n    return 1\n",
            "{ \"sourceRoots\": [\"src\"], \"store\": { \"backend\": \"memory\" } }",
        );
        let error = expect_error(prepare(dir.path(), None));
        assert!(matches!(error, LaunchError::NoEntry), "{error}");
        assert_eq!(error.code(), "dap.launchEntry.blocked");
        assert_eq!(error.status(), "blocked-on-marrow");
        assert_eq!(error.blocked_on(), Some("canonical function-entry facts"));
    }

    #[test]
    fn a_directory_without_a_config_is_no_project() {
        let dir = tempfile::tempdir().unwrap();
        let error = expect_error(prepare(dir.path(), None));
        assert!(matches!(error, LaunchError::NoProject(_)), "{error}");
        assert_eq!(error.code(), "dap.launchProject.invalid");
        assert_eq!(error.status(), "invalid-project");
        assert_eq!(error.blocked_on(), None);
    }
}
