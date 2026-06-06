//! Load a Marrow project for a debug launch.
//!
//! A launch points at a project directory (the one holding `marrow.json`). We
//! parse the config, analyze the sources into a [`CheckedProgram`], and pick the
//! configured default entry. The debugger runs over an in-memory store and refuses
//! projects that still need accepted catalog identity, because DAP is downstream of
//! Marrow's production catalog and store admission APIs.

use std::path::{Path, PathBuf};

use marrow_check::{CheckReport, CheckedProgram, ProjectSources, analyze_project};
use marrow_lsp_core::catalog_admission::requires_accepted_catalog_identity;
use marrow_project::parse_config;
use marrow_store::tree::TreeStore;

/// The project config file whose directory is the project root.
const CONFIG_FILE: &str = "marrow.json";
const STATUS_BLOCKED_ON_MARROW: &str = "blocked-on-marrow";
const STATUS_INVALID_PROJECT: &str = "invalid-project";
const FUNCTION_ENTRY_FACTS: &str = "canonical function-entry facts";
const CATALOG_IDENTITY_FACTS: &str = "accepted catalog identity facts";

/// A loaded project ready to debug: its checked program, the resolved entry
/// function name, and a fresh in-memory store the run will read and write.
pub struct Launch {
    pub program: CheckedProgram,
    pub entry: String,
    pub store: TreeStore,
}

/// Why a launch could not be prepared. Each renders to a single client-facing
/// message on the failed `launch` response.
#[derive(Debug)]
pub enum LaunchError {
    NoProject(PathBuf),
    Config(String),
    Analyze(String),
    NoEntry,
    ExplicitEntryBlocked,
    CatalogIdentityBlocked,
    CheckErrors(Vec<String>),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoProject(dir) => {
                write!(f, "no {CONFIG_FILE} found at {}", dir.display())
            }
            Self::Config(message) => write!(f, "invalid {CONFIG_FILE}: {message}"),
            Self::Analyze(message) => write!(f, "could not analyze the project: {message}"),
            Self::NoEntry => write!(
                f,
                "no entry to run: configure `defaultEntry` in {CONFIG_FILE}; canonical function-entry discovery facts are pending in Marrow"
            ),
            Self::ExplicitEntryBlocked => write!(
                f,
                "blocked-on-marrow: explicit DAP entry strings need canonical function-entry facts from Marrow; configure `defaultEntry` in {CONFIG_FILE}"
            ),
            Self::CatalogIdentityBlocked => write!(
                f,
                "blocked-on-marrow: DAP launch will not establish accepted catalog identity; use Marrow's production catalog flow or wait for read-only debug admission facts"
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
            Self::NoProject(_) | Self::Config(_) | Self::Analyze(_) | Self::CheckErrors(_) => {
                "dap.launchProject.invalid"
            }
            Self::NoEntry | Self::ExplicitEntryBlocked => "dap.launchEntry.blocked",
            Self::CatalogIdentityBlocked => "dap.launchCatalogIdentity.blocked",
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
            Self::CatalogIdentityBlocked => Some(CATALOG_IDENTITY_FACTS),
            Self::NoProject(_) | Self::Config(_) | Self::Analyze(_) | Self::CheckErrors(_) => None,
        }
    }
}

/// Prepare a launch: resolve the project at `project_dir`, analyze it, choose the
/// configured default entry, refuse to run a project with check errors or pending
/// catalog identity, and attach a fresh in-memory store.
pub fn prepare(project_dir: &Path, entry: Option<&str>) -> Result<Launch, LaunchError> {
    if entry.is_some() {
        return Err(LaunchError::ExplicitEntryBlocked);
    }
    let config_path = project_dir.join(CONFIG_FILE);
    if !config_path.is_file() {
        return Err(LaunchError::NoProject(project_dir.to_path_buf()));
    }
    let text = std::fs::read_to_string(&config_path)
        .map_err(|error| LaunchError::Config(error.to_string()))?;
    let config = parse_config(&text).map_err(|error| LaunchError::Config(error.message))?;

    let snapshot = analyze_project(project_dir, &config, &ProjectSources::new())
        .map_err(|error| LaunchError::Analyze(error.to_string()))?;

    // Do not debug a project that does not check: stepping through code the
    // checker rejected would run undefined behavior. Surface the errors instead.
    let errors = check_errors(&snapshot.report);
    if !errors.is_empty() {
        return Err(LaunchError::CheckErrors(errors));
    }
    let program = snapshot.program;
    if requires_accepted_catalog_identity(&program) {
        return Err(LaunchError::CatalogIdentityBlocked);
    }

    let entry = config.default_entry.clone().ok_or(LaunchError::NoEntry)?;

    Ok(Launch {
        program,
        entry,
        store: TreeStore::memory(),
    })
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

    fn write_project(source: &str, config: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("m.mw"), source).unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), config).unwrap();
        dir
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
    fn prepare_blocks_pending_catalog_identity_without_writing_project_state() {
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
        let error = expect_error(prepare(dir.path(), None));
        assert!(
            matches!(error, LaunchError::CatalogIdentityBlocked),
            "{error}"
        );
        assert_eq!(error.code(), "dap.launchCatalogIdentity.blocked");
        assert_eq!(error.status(), "blocked-on-marrow");
        assert_eq!(error.blocked_on(), Some("accepted catalog identity facts"));
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
    fn an_explicit_entry_is_blocked_until_marrow_exposes_entry_facts() {
        let dir = write_project(
            "module m\n\npub fn main(): int\n    return 1\n\npub fn other(): int\n    return 2\n",
            "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"m::main\" } }",
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
            "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"m::main\" } }",
        );
        let error = expect_error(prepare(dir.path(), None));
        assert!(matches!(error, LaunchError::CheckErrors(_)), "{error}");
    }

    #[test]
    fn missing_entry_error_frames_the_blocked_entry_contract() {
        let dir = write_project(
            "module m\n\npub fn main(): int\n    return 1\n",
            "{ \"sourceRoots\": [\"src\"] }",
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
