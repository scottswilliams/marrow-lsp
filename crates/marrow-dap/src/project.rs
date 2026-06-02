//! Load a Marrow project for a debug launch and open its store for writing.
//!
//! A launch points at a project directory (the one holding `marrow.json`). We
//! parse the config, analyze the sources into a [`CheckedProgram`], pick the entry
//! function, and open the project's store. This is the one place in all of
//! marrow-lsp that opens a real store for *writing*: a debug run may write managed
//! data, and the debugger shows those writes live. The write store is opened only
//! here, only on an explicit launch.

use std::path::{Path, PathBuf};

use marrow_check::{CheckedProgram, ProjectSources, analyze_project};
use marrow_project::{ProjectConfig, StoreBackend, parse_config};
use marrow_store::mem::MemStore;
use marrow_store::redb::RedbStore;

/// The project config file whose directory is the project root.
const CONFIG_FILE: &str = "marrow.json";
/// A native store's file name inside its data directory.
const STORE_FILE: &str = "marrow.redb";

/// A loaded project ready to debug: its checked program, the resolved entry
/// function name, and a freshly opened store the run will read and write.
pub struct Launch {
    pub program: CheckedProgram,
    pub entry: String,
    pub store: LaunchStore,
}

/// The store a debug run uses, as one of the two concrete backends. Kept concrete
/// (not a `Box<dyn Backend>`) so the run-thread can unsize a `&RefCell<_>` of it to
/// the `&RefCell<dyn Backend>` the runtime wants — a trait object behind a `Box`
/// could not be placed directly in a `RefCell<dyn Backend>`.
pub enum LaunchStore {
    Memory(MemStore),
    // The redb handle is large; box it so the enum is not dominated by it.
    Native(Box<RedbStore>),
}

/// Why a launch could not be prepared. Each renders to a single client-facing
/// message on the failed `launch` response.
#[derive(Debug)]
pub enum LaunchError {
    NoProject(PathBuf),
    Config(String),
    Analyze(String),
    NoEntry,
    Store(String),
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
                "no entry to run: pass `entry` (\"module::fn\") or configure `defaultEntry` in {CONFIG_FILE}; this debug/admin prototype entry string is blocked on canonical function-entry facts, not a stable production entry API"
            ),
            Self::Store(message) => write!(f, "could not open the store: {message}"),
            Self::CheckErrors(errors) => {
                write!(f, "the project has errors: {}", errors.join("; "))
            }
        }
    }
}

/// Prepare a launch: resolve the project at `project_dir`, analyze it, choose the
/// entry (`entry` argument or the configured default), refuse to run a project
/// with check errors, and open its store for writing.
pub fn prepare(project_dir: &Path, entry: Option<&str>) -> Result<Launch, LaunchError> {
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
    let errors: Vec<String> = snapshot
        .report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == marrow_syntax::Severity::Error)
        .map(|diagnostic| diagnostic.message.clone())
        .collect();
    if !errors.is_empty() {
        return Err(LaunchError::CheckErrors(errors));
    }

    let entry = entry
        .map(str::to_string)
        .or_else(|| config.default_entry.clone())
        .ok_or(LaunchError::NoEntry)?;

    let store = open_store(project_dir, &config)?;
    Ok(Launch {
        program: snapshot.program,
        entry,
        store,
    })
}

/// Open the project's store for writing: the native redb file under its data
/// directory, or a fresh in-memory store for a memory-backed (or store-less)
/// project. The redb path is created if absent, so a first debug run of a new
/// project just works.
fn open_store(project_dir: &Path, config: &ProjectConfig) -> Result<LaunchStore, LaunchError> {
    let Some(store) = config.store.as_ref() else {
        return Ok(LaunchStore::Memory(MemStore::new()));
    };
    match store.backend {
        StoreBackend::Memory => Ok(LaunchStore::Memory(MemStore::new())),
        StoreBackend::Native => {
            let data_dir = store
                .data_dir
                .as_deref()
                .ok_or_else(|| LaunchError::Store("native store has no data directory".into()))?;
            let dir = project_dir.join(data_dir);
            std::fs::create_dir_all(&dir).map_err(|error| LaunchError::Store(error.to_string()))?;
            let path = dir.join(STORE_FILE);
            let store = RedbStore::open(&path)
                .map_err(|error| LaunchError::Store(error.code().to_string()))?;
            Ok(LaunchStore::Native(Box::new(store)))
        }
    }
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
    fn prepare_resolves_the_default_entry_and_opens_a_memory_store() {
        let dir = write_project(
            "module m\n\npub fn main(): int\n    return 1\n",
            "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"m::main\" }, \"store\": { \"backend\": \"memory\" } }",
        );
        let launch = prepare(dir.path(), None).unwrap();
        assert_eq!(launch.entry, "m::main");
    }

    #[test]
    fn an_explicit_entry_overrides_the_default() {
        let dir = write_project(
            "module m\n\npub fn main(): int\n    return 1\n\npub fn other(): int\n    return 2\n",
            "{ \"sourceRoots\": [\"src\"], \"run\": { \"defaultEntry\": \"m::main\" } }",
        );
        let launch = prepare(dir.path(), Some("m::other")).unwrap();
        assert_eq!(launch.entry, "m::other");
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
    fn missing_entry_error_frames_the_prototype_entry_contract() {
        let dir = write_project(
            "module m\n\npub fn main(): int\n    return 1\n",
            "{ \"sourceRoots\": [\"src\"] }",
        );
        let error = expect_error(prepare(dir.path(), None));
        assert!(matches!(error, LaunchError::NoEntry), "{error}");
        let message = error.to_string();
        assert!(
            message.contains("debug/admin prototype entry string"),
            "missing entry should frame entry as a prototype string: {message}"
        );
        assert!(
            message.contains("canonical function-entry facts"),
            "missing entry should name the blocking facts: {message}"
        );
        assert!(
            message.contains("not a stable production entry API"),
            "missing entry should not imply a stable entry API: {message}"
        );
        assert!(
            message.contains("pass `entry`"),
            "missing entry should preserve explicit entry guidance: {message}"
        );
        assert!(
            message.contains("defaultEntry"),
            "missing entry should preserve defaultEntry guidance: {message}"
        );
    }

    #[test]
    fn a_directory_without_a_config_is_no_project() {
        let dir = tempfile::tempdir().unwrap();
        let error = expect_error(prepare(dir.path(), None));
        assert!(matches!(error, LaunchError::NoProject(_)), "{error}");
    }
}
