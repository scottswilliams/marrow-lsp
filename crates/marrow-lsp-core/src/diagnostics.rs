//! Turn a project analysis into LSP diagnostics, grouped by file.
//!
//! Marrow reports problems with UTF-8 byte spans against the project's files; an
//! editor wants `lsp_types::Diagnostic` ranges in UTF-16 columns, keyed by the
//! `file://` URL of each open buffer. This module bridges the two, reusing each
//! file's [`LineIndex`] so a span lands on the same character the editor sees.

use std::collections::HashMap;
use std::path::Path;

use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Url};
use marrow_check::{AnalysisSnapshot, CheckDiagnostic};
use marrow_syntax::{Severity, SourceSpan};

use crate::positions::LineIndex;

/// The diagnostic `source` an editor shows beside each Marrow problem.
pub const SOURCE: &str = "marrow";

/// Map one Marrow diagnostic to its LSP form against the file's [`LineIndex`].
///
/// The span's byte range becomes a UTF-16 range; the dotted code becomes a string
/// code; and `help`, when present, is appended to the message as its own
/// `help: …` line so the guidance survives in clients that show only the message.
/// `help` is carried separately because [`CheckDiagnostic`] does not retain it —
/// the appending lives here so it is exercised and stays in one place.
pub fn diagnostic_to_lsp(
    code: &str,
    severity: Severity,
    span: SourceSpan,
    message: &str,
    help: Option<&str>,
    index: &LineIndex,
) -> Diagnostic {
    let message = match help {
        Some(help) => format!("{message}\nhelp: {help}"),
        None => message.to_string(),
    };
    Diagnostic {
        range: index.range(span.start_byte, span.end_byte),
        severity: Some(severity_to_lsp(severity)),
        code: Some(NumberOrString::String(code.to_string())),
        source: Some(SOURCE.to_string()),
        message,
        ..Diagnostic::default()
    }
}

/// Group an analysis snapshot's diagnostics into per-file LSP diagnostics.
///
/// Each diagnostic is placed under the `file://` URL of its `.file`, with its
/// range computed from that file's source: the open buffer's index when one is
/// supplied by `index_for`, otherwise an index built from the file's on-disk text
/// (or empty text when it cannot be read, which keeps ranges valid). Every URL the
/// caller knows is open gets an entry — an empty `Vec` when it has no diagnostics —
/// so publishing the map clears any problems the last edit fixed.
pub fn snapshot_to_diagnostics<'a>(
    snapshot: &AnalysisSnapshot,
    open_urls: impl IntoIterator<Item = &'a Url>,
    index_for: impl Fn(&Url) -> Option<&'a LineIndex>,
) -> HashMap<Url, Vec<Diagnostic>> {
    let mut result: HashMap<Url, Vec<Diagnostic>> = open_urls
        .into_iter()
        .map(|url| (url.clone(), Vec::new()))
        .collect();

    // Build a disk-backed index on demand for a file that is reported but not
    // open, so its diagnostics still land on the right characters.
    let mut disk_indices: HashMap<Url, LineIndex> = HashMap::new();

    for diagnostic in &snapshot.report.diagnostics {
        let Some(url) = path_to_url(&diagnostic.file) else {
            continue;
        };
        let index = match index_for(&url) {
            Some(index) => index,
            None => disk_indices
                .entry(url.clone())
                .or_insert_with(|| LineIndex::new(read_to_string(&diagnostic.file))),
        };
        result
            .entry(url)
            .or_default()
            .push(check_diagnostic_to_lsp(diagnostic, index));
    }

    result
}

/// Map a real [`CheckDiagnostic`]. It carries no `help`, so none is appended.
fn check_diagnostic_to_lsp(diagnostic: &CheckDiagnostic, index: &LineIndex) -> Diagnostic {
    diagnostic_to_lsp(
        diagnostic.code,
        diagnostic.severity,
        diagnostic.span,
        &diagnostic.message,
        None,
        index,
    )
}

fn severity_to_lsp(severity: Severity) -> DiagnosticSeverity {
    match severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
    }
}

/// The `file://` URL for an absolute path, or `None` for a path that cannot form
/// one (a relative path has no file URL).
pub fn path_to_url(path: &Path) -> Option<Url> {
    Url::from_file_path(path).ok()
}

fn read_to_string(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn span(start_byte: usize, end_byte: usize, line: u32, column: u32) -> SourceSpan {
        SourceSpan {
            start_byte,
            end_byte,
            line,
            column,
        }
    }

    #[test]
    fn maps_range_severity_code_and_source() {
        let index = LineIndex::new("let x = 1\nlet y = 2\n");
        let diagnostic = diagnostic_to_lsp(
            "check.unknown_type",
            Severity::Error,
            span(10, 13, 1, 0),
            "unknown type `Foo`",
            None,
            &index,
        );

        assert_eq!(diagnostic.range.start, lsp_types::Position::new(1, 0));
        assert_eq!(diagnostic.range.end, lsp_types::Position::new(1, 3));
        assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(
            diagnostic.code,
            Some(NumberOrString::String("check.unknown_type".to_string()))
        );
        assert_eq!(diagnostic.source.as_deref(), Some("marrow"));
        assert_eq!(diagnostic.message, "unknown type `Foo`");
    }

    #[test]
    fn warning_severity_maps_to_warning() {
        let index = LineIndex::new("x");
        let diagnostic = diagnostic_to_lsp(
            "check.something",
            Severity::Warning,
            span(0, 1, 0, 0),
            "watch out",
            None,
            &index,
        );
        assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::WARNING));
    }

    #[test]
    fn help_is_appended_as_its_own_line() {
        let index = LineIndex::new("x = 1");
        let diagnostic = diagnostic_to_lsp(
            "parse.operator",
            Severity::Error,
            span(0, 1, 0, 0),
            "use `and`, not `&&`",
            Some("Marrow spells logical and as `and`"),
            &index,
        );
        assert_eq!(
            diagnostic.message,
            "use `and`, not `&&`\nhelp: Marrow spells logical and as `and`"
        );
    }

    #[test]
    fn groups_diagnostics_by_file_and_clears_clean_open_docs() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.mw");
        let clean = dir.path().join("clean.mw");
        std::fs::write(&bad, "first line\nsecond line\n").unwrap();
        std::fs::write(&clean, "ok\n").unwrap();

        let bad_url = path_to_url(&bad).unwrap();
        let clean_url = path_to_url(&clean).unwrap();

        let snapshot = AnalysisSnapshot {
            report: marrow_check::CheckReport {
                diagnostics: vec![CheckDiagnostic {
                    code: "check.unknown_type",
                    severity: Severity::Error,
                    file: bad.clone(),
                    message: "unknown type `Foo`".to_string(),
                    span: span(11, 17, 1, 0),
                }],
            },
            program: marrow_check::CheckedProgram::default(),
            files: Vec::new(),
        };

        // Both files are open; neither supplies an in-memory index, so the disk
        // text backs the range.
        let open = [bad_url.clone(), clean_url.clone()];
        let result = snapshot_to_diagnostics(&snapshot, &open, |_| None);

        assert_eq!(result[&bad_url].len(), 1);
        assert_eq!(
            result[&bad_url][0].range.start,
            lsp_types::Position::new(1, 0)
        );
        // A clean open document is present with an empty Vec so its problems clear.
        assert_eq!(result[&clean_url], Vec::<Diagnostic>::new());
    }

    #[test]
    fn relative_path_is_skipped() {
        let snapshot = AnalysisSnapshot {
            report: marrow_check::CheckReport {
                diagnostics: vec![CheckDiagnostic {
                    code: "check.unknown_type",
                    severity: Severity::Error,
                    file: PathBuf::from("relative.mw"),
                    message: "unknown type".to_string(),
                    span: span(0, 1, 0, 0),
                }],
            },
            program: marrow_check::CheckedProgram::default(),
            files: Vec::new(),
        };
        let result = snapshot_to_diagnostics(&snapshot, std::iter::empty(), |_| None);
        assert!(result.is_empty());
    }
}
