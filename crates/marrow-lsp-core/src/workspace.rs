//! Resolve a Marrow project from a file path and check it against open buffers.
//!
//! A project is the directory holding a `marrow.json`. Given a file inside one,
//! the workspace walks up to that file, parses the config, overlays every open
//! buffer under the root onto disk, and runs the project checker — so analysis
//! reflects unsaved edits, matching what `marrow check` would report once saved.

use std::path::{Path, PathBuf};

use lsp_types::Url;
use marrow_check::{AnalysisSnapshot, ProjectSources, analyze_project};
use marrow_project::{ProjectConfig, parse_config};

use crate::documents::Documents;

/// The name of a Marrow project's configuration file; its directory is the root.
const CONFIG_FILE: &str = "marrow.json";

/// A resolved project: its root directory and parsed configuration.
#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub config: ProjectConfig,
}

/// Why a file could not be resolved to a checkable project.
#[derive(Debug)]
pub enum WorkspaceError {
    /// No `marrow.json` was found walking up from the file.
    NoProject,
    /// A `marrow.json` was found but could not be parsed.
    Config(marrow_project::ConfigError),
    /// The project's source roots could not be walked.
    Discover(marrow_project::DiscoverError),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoProject => write!(f, "no marrow.json found for the file"),
            Self::Config(error) => write!(f, "{error}"),
            Self::Discover(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

/// Holds the project resolved for the buffers in play and its latest analysis.
#[derive(Default)]
pub struct Workspace {
    project: Option<Project>,
    latest: Option<AnalysisSnapshot>,
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    /// The most recent analysis, if any, so a later request can read the cached
    /// snapshot without recomputing.
    pub fn latest(&self) -> Option<&AnalysisSnapshot> {
        self.latest.as_ref()
    }

    /// Resolve the project for `file` (if not already resolved), overlay every open
    /// buffer under the root, run the checker, cache the snapshot, and return a
    /// reference to it.
    ///
    /// Resolution is sticky: once a project is found it is reused, since every open
    /// `.mw` buffer in a session belongs to the same project. A file outside any
    /// project yields [`WorkspaceError::NoProject`].
    pub fn recompute(
        &mut self,
        file: &Path,
        documents: &Documents,
    ) -> Result<&AnalysisSnapshot, WorkspaceError> {
        if self.project.is_none() {
            self.project = Some(resolve_project(file)?);
        }
        let project = self.project.as_ref().expect("project resolved just above");

        let sources = overlay(&project.root, documents);
        let snapshot = analyze_project(&project.root, &project.config, &sources)
            .map_err(WorkspaceError::Discover)?;

        self.latest = Some(snapshot);
        Ok(self.latest.as_ref().expect("just stored a snapshot"))
    }
}

/// Walk up from `file` to the nearest directory holding a `marrow.json`, parse it,
/// and return the project rooted there.
fn resolve_project(file: &Path) -> Result<Project, WorkspaceError> {
    let mut directory = file.parent();
    while let Some(current) = directory {
        let config_path = current.join(CONFIG_FILE);
        if config_path.is_file() {
            let text = std::fs::read_to_string(&config_path).map_err(|error| {
                WorkspaceError::Config(marrow_project::ConfigError {
                    code: marrow_project::CONFIG_INVALID,
                    message: error.to_string(),
                })
            })?;
            let config = parse_config(&text).map_err(WorkspaceError::Config)?;
            return Ok(Project {
                root: current.to_path_buf(),
                config,
            });
        }
        directory = current.parent();
    }
    Err(WorkspaceError::NoProject)
}

/// Build a source overlay from every open buffer whose file lives under `root`.
/// A buffer outside the project is left out so it cannot perturb the analysis.
fn overlay(root: &Path, documents: &Documents) -> ProjectSources {
    let mut sources = ProjectSources::new();
    for (url, document) in documents.iter() {
        if let Some(path) = url_to_path(url)
            && path.starts_with(root)
        {
            sources.insert(path, document.text.clone());
        }
    }
    sources
}

/// The filesystem path of a `file://` URL, or `None` for any other scheme.
pub fn url_to_path(url: &Url) -> Option<PathBuf> {
    url.to_file_path().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::{path_to_url, snapshot_to_diagnostics};

    /// A project whose one library file is clean on disk; an open buffer overlays a
    /// version with a type error. Analysis must report the error against the buffer
    /// while the same project checked from disk alone reports none.
    #[test]
    fn open_buffer_overlay_surfaces_an_error_the_disk_file_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("a.mw");
        // A clean module: a value-returning function that returns an int.
        std::fs::write(&file, "module a\n\npub fn answer(): int\n    return 42\n").unwrap();

        // From disk alone, no diagnostics.
        let mut clean = Workspace::new();
        let empty = Documents::new();
        let clean_snapshot = clean.recompute(&file, &empty).unwrap();
        assert!(
            clean_snapshot.report.diagnostics.is_empty(),
            "clean on-disk project should have no diagnostics, got {:?}",
            clean_snapshot.report.diagnostics
        );

        // Overlay a buffer whose body returns a string where an int is declared.
        let url = path_to_url(&file).unwrap();
        let mut documents = Documents::new();
        documents.open(
            url.clone(),
            1,
            "module a\n\npub fn answer(): int\n    return \"nope\"\n".to_string(),
        );

        let mut workspace = Workspace::new();
        let snapshot = workspace.recompute(&file, &documents).unwrap();
        assert!(
            !snapshot.report.diagnostics.is_empty(),
            "overlay with a type error should report a diagnostic"
        );

        // The error is published for the buffer's URL.
        let urls: Vec<Url> = documents.urls().cloned().collect();
        let published =
            snapshot_to_diagnostics(snapshot, &urls, |u| documents.get(u).map(|d| &d.index));
        let diagnostics = &published[&url];
        assert!(
            diagnostics.iter().any(|d| matches!(
                &d.code,
                Some(lsp_types::NumberOrString::String(code)) if code == "check.return_type"
            )),
            "expected a check.return_type diagnostic, got {diagnostics:?}"
        );
    }

    #[test]
    fn a_file_outside_any_project_has_no_project() {
        let dir = tempfile::tempdir().unwrap();
        let stray = dir.path().join("loose.mw");
        std::fs::write(&stray, "module loose\n").unwrap();
        let mut workspace = Workspace::new();
        let documents = Documents::new();
        assert!(matches!(
            workspace.recompute(&stray, &documents),
            Err(WorkspaceError::NoProject)
        ));
    }
}
