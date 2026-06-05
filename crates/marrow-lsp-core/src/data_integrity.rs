//! The schema-change-impact advisory: a capped, on-demand scan of a project's
//! saved data that flags every stored record the *current* schema can no longer
//! account for.
//!
//! The scan is bounded and on demand only. It delegates integrity semantics to
//! Marrow's canonical tooling visitor.

use serde::{Deserialize, Serialize};

use marrow_check::CheckedProgram;
use marrow_check::tooling::{self, IntegrityProblem};
use marrow_store::StoreError;

use crate::store::{Availability, LiveStore};

/// The most stored entries one scan reads before reporting the rest as
/// `truncated`. The advisory is a fast, on-demand health check, not a full audit,
/// so it walks a bounded prefix of the tree rather than an unbounded store.
const INTEGRITY_SCAN_LIMIT: usize = 5000;

/// One stored record the current schema cannot account for: the human path text,
/// Marrow tooling problem code, message, and optional help text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// The canonical Marrow path text (`^books(1).title`), or `?<hex>` of the raw
    /// key when it does not decode, so a corrupt key stays visible.
    pub path: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

/// The result of a schema-change-impact scan: every finding, how many entries were
/// scanned, and whether the store held more past the cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaImpact {
    pub findings: Vec<Finding>,
    pub scanned: usize,
    pub truncated: bool,
}

/// `marrow/dataIntegrity` request params: none. The advisory always scans the
/// whole store, so the request carries no arguments; an empty object decodes here.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataIntegrityParams {}

/// `marrow/dataIntegrity` reply: whether the store could be read, plus the scan's
/// findings, the count scanned, and whether the store held more past the cap. When
/// `available` is false the findings are empty and the editor shows an unavailable
/// state rather than an error — the same soft-degrade the data-roots view uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataIntegrityResult {
    pub available: bool,
    pub findings: Vec<Finding>,
    pub scanned: usize,
    pub truncated: bool,
}

impl DataIntegrityResult {
    fn unavailable() -> Self {
        Self {
            available: false,
            findings: Vec::new(),
            scanned: 0,
            truncated: false,
        }
    }
}

/// Run the schema-change-impact advisory for the LSP custom request, soft-degrading
/// a missing reader or an unreadable store to an `available: false` envelope. The
/// gating (live data on/off) is the transport's responsibility, mirroring the Data
/// Explorer: the transport passes `None` when live data is off so the store is
/// never opened.
pub fn data_integrity(reader: Option<&LiveStore>, program: &CheckedProgram) -> DataIntegrityResult {
    let Some(reader) = reader else {
        return DataIntegrityResult::unavailable();
    };
    match scan_data_integrity(reader, program, INTEGRITY_SCAN_LIMIT) {
        Availability::Available(SchemaImpact {
            findings,
            scanned,
            truncated,
        }) => DataIntegrityResult {
            available: true,
            findings,
            scanned,
            truncated,
        },
        Availability::Unavailable => DataIntegrityResult::unavailable(),
    }
}

/// Scan the project's saved data against the current schema and flag every stored
/// record the schema can no longer account for.
fn scan_data_integrity(
    reader: &LiveStore,
    program: &CheckedProgram,
    limit: usize,
) -> Availability<SchemaImpact> {
    let limit = limit.min(INTEGRITY_SCAN_LIMIT);
    reader.with_tree_result(|store| {
        let mut findings = Vec::new();
        let mut scanned = 0usize;
        let mut truncated = false;
        let result = tooling::visit_integrity_problems(store, program, |outcome| {
            if scanned == limit {
                truncated = true;
                return Err(StoreError::LimitExceeded {
                    limit: "data integrity preview",
                });
            }
            scanned += 1;
            if let Some(problem) = outcome.problem {
                findings.push(finding_for(problem));
            }
            Ok(())
        });
        match result {
            Ok(()) => Ok(SchemaImpact {
                findings,
                scanned,
                truncated: false,
            }),
            Err(StoreError::LimitExceeded {
                limit: "data integrity preview",
            }) if truncated => Ok(SchemaImpact {
                findings,
                scanned,
                truncated: true,
            }),
            Err(error) => Err(error),
        }
    })
}

fn finding_for(problem: IntegrityProblem) -> Finding {
    Finding {
        path: problem.path,
        code: problem.code.to_string(),
        message: problem.message,
        help: problem.help.map(str::to_string),
    }
}
