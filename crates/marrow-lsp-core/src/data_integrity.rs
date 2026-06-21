//! The schema-change-impact advisory: a capped, on-demand scan of a project's
//! saved data that flags every stored record the *current* schema can no longer
//! account for.
//!
//! The scan is bounded and on demand only. It delegates integrity semantics to
//! Marrow's canonical tooling sample.

use serde::Serialize;

use marrow_check::tooling::IntegrityProblem;
use marrow_json::DataGenerationJson;

use crate::store::SavedDataSession;

/// The most stored entries one scan reads before reporting the rest as
/// `truncated`. The advisory is a fast, on-demand health check, not a full audit,
/// so it walks a bounded prefix of the tree rather than an unbounded store.
const INTEGRITY_SCAN_LIMIT: usize = 5000;

/// One stored record the current schema cannot account for: the human path text,
/// Marrow tooling problem code, message, and optional help text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// The canonical Marrow path text (`^books(1).title`), or `?<hex>` of the raw
    /// key when it does not decode, so a corrupt key stays visible.
    pub path: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

/// `marrow/dataIntegrity` reply: whether the store could be read, plus the scan's
/// findings, the count scanned, and whether the store held more past the cap. When
/// `available` is false the findings are empty and the editor shows an unavailable
/// state rather than an error — the same soft-degrade the data-roots view uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataIntegrityResult {
    pub available: bool,
    pub findings: Vec<Finding>,
    pub scanned: usize,
    pub truncated: bool,
    pub store_snapshot: Option<DataGenerationJson>,
}

impl DataIntegrityResult {
    fn unavailable() -> Self {
        Self {
            available: false,
            findings: Vec::new(),
            scanned: 0,
            truncated: false,
            store_snapshot: None,
        }
    }
}

/// Run the schema-change-impact advisory for the LSP custom request,
/// soft-degrading a missing session or unreadable store to an `available: false`
/// envelope. The transport passes `None` when live data is off or the
/// editor-visible source is stale, so the store is never opened in those cases.
pub fn data_integrity(session: Option<&SavedDataSession>) -> DataIntegrityResult {
    let Some(session) = session else {
        return DataIntegrityResult::unavailable();
    };
    match session
        .surface_read()
        .saved_data_integrity_sample(INTEGRITY_SCAN_LIMIT)
    {
        Ok(stamped) => DataIntegrityResult {
            available: true,
            findings: stamped.data.problems.into_iter().map(finding_for).collect(),
            scanned: stamped.data.items_checked,
            truncated: stamped.data.truncated,
            store_snapshot: Some(DataGenerationJson::from(&stamped.stamp)),
        },
        Err(_) => DataIntegrityResult::unavailable(),
    }
}

fn finding_for(problem: IntegrityProblem) -> Finding {
    Finding {
        path: problem.path,
        code: problem.code.to_string(),
        message: problem.message,
        help: problem.help.map(str::to_string),
    }
}
