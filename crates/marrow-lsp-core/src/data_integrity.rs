//! The schema-change-impact advisory: a capped, on-demand scan of a project's
//! saved data that flags every stored record the *current* schema can no longer
//! account for.
//!
//! After a developer edits a resource's schema, some records already in the store
//! may no longer fit it: a record saved under a root or field the schema no longer
//! declares is now an **orphan**, and a value whose declared scalar type changed
//! may no longer **decode**. This is exactly the check `marrow data integrity`
//! runs from the CLI, reusing the same classification — [`classify_saved_path`]
//! and [`decode_value`] — so the editor advisory and the CLI never diverge on what
//! counts as a problem.
//!
//! The scan is bounded ([`SCAN_CAP`]) and on demand only: it opens the store
//! through a short-lived [`StoreReader`], reads one capped page, and drops the
//! handle. It is never run on save or on edit, and an unreadable store degrades to
//! [`Availability::Unavailable`] like every other store inspection.

use serde::{Deserialize, Serialize};

use marrow_check::CheckedProgram;
use marrow_run::{SavedPathClass, classify_saved_path};
use marrow_store::path::{decode_path, display_path};
use marrow_store::value::decode_value;

use crate::store::{Availability, StoreReader};

/// The most stored entries one [`schema_impact`] scan reads before reporting the
/// rest as `truncated`. The advisory is a fast, on-demand health check, not a full
/// audit, so it walks a bounded prefix of the tree rather than an unbounded store.
pub const SCAN_CAP: usize = 5000;

/// What kind of mismatch a finding reports. Mirrors the [`SavedPathClass`]
/// outcomes `marrow data integrity` treats as problems; a declared scalar that
/// decodes and a generated index marker are healthy and produce no finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// The path is under no declared root, or names a member the schema does not
    /// declare: stale or foreign data the current schema cannot account for.
    Orphan,
    /// The path is a declared scalar leaf, but its stored bytes are not a canonical
    /// form of that scalar's type — typically because the field's declared type
    /// changed under existing data.
    Undecodable,
    /// The member chain resolves, but a record or index key is stored at a scalar
    /// type the schema does not declare for that key position — a corrupt keyspace,
    /// distinct from an orphan.
    KeyTypeMismatch,
}

/// One stored record the current schema cannot account for: the human path text,
/// the kind of mismatch, a one-line message, and the declared scalar type name for
/// an undecodable value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// The canonical Marrow path text (`^books(1).title`), or `?<hex>` of the raw
    /// key when it does not decode, so a corrupt key stays visible.
    pub path: String,
    pub kind: FindingKind,
    pub message: String,
    /// The declared scalar type name (`int`, `string`, …) for an
    /// [`FindingKind::Undecodable`] finding; absent for an orphan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
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
/// state rather than an error — the same soft-degrade the Data Explorer uses.
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
pub fn data_integrity(
    reader: Option<&StoreReader>,
    program: &CheckedProgram,
) -> DataIntegrityResult {
    let Some(reader) = reader else {
        return DataIntegrityResult::unavailable();
    };
    match schema_impact(reader, program, SCAN_CAP) {
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
///
/// Reads one capped page (`limit`, clamped to at most [`SCAN_CAP`]) over the whole
/// tree, then classifies each stored `(key, value)` exactly as
/// `marrow data integrity` does: a path that does not decode, or that
/// [`classify_saved_path`] places as an [`SavedPathClass::Orphan`], is flagged; a
/// declared [`SavedPathClass::Scalar`] whose value fails [`decode_value`] is
/// flagged as undecodable; a healthy scalar or a generated index marker is not.
///
/// An unreadable store (locked, corrupt, absent, or unsupported) yields
/// [`Availability::Unavailable`], never an error.
pub fn schema_impact(
    reader: &StoreReader,
    program: &CheckedProgram,
    limit: usize,
) -> Availability<SchemaImpact> {
    let limit = limit.min(SCAN_CAP);
    match reader.scan(limit) {
        Availability::Available(page) => {
            let findings = page
                .entries
                .iter()
                .filter_map(|(key, value)| finding_for(program, key, value))
                .collect();
            Availability::Available(SchemaImpact {
                findings,
                scanned: page.entries.len(),
                truncated: page.truncated,
            })
        }
        Availability::Unavailable => Availability::Unavailable,
    }
}

/// Classify one stored `(key, value)` against the schema, returning a finding when
/// the schema cannot account for it. Composes the same primitives the CLI's
/// `check_record` does so the two surfaces agree on every verdict.
fn finding_for(program: &CheckedProgram, key: &[u8], value: &[u8]) -> Option<Finding> {
    let Some(segments) = decode_path(key) else {
        // A key that does not decode is corrupt store data the schema certainly
        // cannot account for; surface it as an orphan with the raw key visible.
        return Some(Finding {
            path: display_path(key),
            kind: FindingKind::Orphan,
            message: "stored key is not a well-formed saved path".to_string(),
            r#type: None,
        });
    };
    match classify_saved_path(program, &segments) {
        SavedPathClass::Scalar(ty) => {
            if decode_value(value, ty).is_some() {
                None
            } else {
                Some(Finding {
                    path: display_path(key),
                    kind: FindingKind::Undecodable,
                    message: format!("stored value no longer decodes as {}", ty.name()),
                    r#type: Some(ty.name().to_string()),
                })
            }
        }
        // Generated index entries are raw-only by design; they are legal.
        SavedPathClass::IndexMarker => None,
        // A key written at the wrong scalar type: the member is real but the
        // keyspace is corrupt. Mirrors `marrow data integrity`'s `data.key_type`.
        SavedPathClass::KeyTypeMismatch { expected, found } => Some(Finding {
            path: display_path(key),
            kind: FindingKind::KeyTypeMismatch,
            message: format!(
                "stored key is a {} where the schema declares {}",
                found.name(),
                expected.name()
            ),
            r#type: None,
        }),
        SavedPathClass::Orphan => Some(Finding {
            path: display_path(key),
            kind: FindingKind::Orphan,
            message: "saved under a root or member the current schema does not declare".to_string(),
            r#type: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Project;
    use marrow_check::{ProjectSources, analyze_project};
    use marrow_project::parse_config;
    use marrow_store::backend::Backend;
    use marrow_store::path::{PathSegment, SavedKey, encode_path};
    use marrow_store::redb::RedbStore;
    use std::path::{Path, PathBuf};

    /// A project pinning a native store under `data/`, with one `Book` resource
    /// saved at `^books(id: int)` carrying a required `title: string`. Returns the
    /// project, its checked program, and the path the store file lives at.
    fn project_with_books() -> (Project, CheckedProgram, PathBuf) {
        let source = "\
module shelf

resource Book at ^books(id: int)
    required title: string
";
        build_project(source)
    }

    /// Build a project whose single `src/shelf.mw` is `source`, pinning a native
    /// store under `data/`.
    fn build_project(source: &str) -> (Project, CheckedProgram, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let config_text = r#"{
            "sourceRoots": ["src"],
            "store": { "backend": "native", "dataDir": "data" }
        }"#;
        std::fs::write(root.join("marrow.json"), config_text).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(src.join("shelf.mw"), source).unwrap();

        let config = parse_config(config_text).unwrap();
        let snapshot = analyze_project(&root, &config, &ProjectSources::new()).unwrap();
        let project = Project {
            root: root.clone(),
            config,
        };
        let store_path = root.join("data").join("marrow.redb");
        // Keep the temp dir for the test's lifetime; the OS reclaims it on exit.
        std::mem::forget(dir);
        (project, snapshot.program, store_path)
    }

    /// `^books(id).title`.
    fn book_title(id: i64) -> Vec<u8> {
        encode_path(&[
            PathSegment::Root("books".to_string()),
            PathSegment::RecordKey(SavedKey::Int(id)),
            PathSegment::Field("title".to_string()),
        ])
    }

    fn write_entries(path: &Path, entries: &[(Vec<u8>, Vec<u8>)]) {
        let mut store = RedbStore::open(path).unwrap();
        for (key, value) in entries {
            store.write(key, value.clone()).unwrap();
        }
        // The store drops here, releasing the writer lock so a read-only open can
        // take the file.
    }

    fn impact(project: &Project, program: &CheckedProgram) -> SchemaImpact {
        let reader = StoreReader::for_project(project).expect("native store has a reader");
        schema_impact(&reader, program, SCAN_CAP)
            .into_option()
            .expect("store is readable")
    }

    #[test]
    fn a_clean_store_yields_no_findings() {
        let (project, program, path) = project_with_books();
        write_entries(&path, &[(book_title(1), b"Mort".to_vec())]);
        let result = impact(&project, &program);
        assert!(
            result.findings.is_empty(),
            "a store that matches the schema is clean: {result:?}"
        );
        assert_eq!(result.scanned, 1);
        assert!(!result.truncated);
    }

    #[test]
    fn data_under_an_undeclared_root_is_flagged_as_orphan() {
        // The schema declares `^books`, but the store also holds `^authors`, a root
        // the current schema does not declare.
        let (project, program, path) = project_with_books();
        let orphan = encode_path(&[
            PathSegment::Root("authors".to_string()),
            PathSegment::RecordKey(SavedKey::Int(1)),
            PathSegment::Field("name".to_string()),
        ]);
        write_entries(
            &path,
            &[
                (book_title(1), b"Mort".to_vec()),
                (orphan, b"Pratchett".to_vec()),
            ],
        );
        let result = impact(&project, &program);
        assert_eq!(result.findings.len(), 1, "one orphan: {result:?}");
        let finding = &result.findings[0];
        assert_eq!(finding.kind, FindingKind::Orphan);
        assert_eq!(finding.path, "^authors(1).name");
        assert!(finding.r#type.is_none());
    }

    #[test]
    fn data_under_an_undeclared_field_is_flagged_as_orphan() {
        // `^books(1).subtitle` names a field the current schema does not declare.
        let (project, program, path) = project_with_books();
        let orphan_field = encode_path(&[
            PathSegment::Root("books".to_string()),
            PathSegment::RecordKey(SavedKey::Int(1)),
            PathSegment::Field("subtitle".to_string()),
        ]);
        write_entries(&path, &[(orphan_field, b"A Discworld Novel".to_vec())]);
        let result = impact(&project, &program);
        assert_eq!(result.findings.len(), 1, "{result:?}");
        assert_eq!(result.findings[0].kind, FindingKind::Orphan);
        assert_eq!(result.findings[0].path, "^books(1).subtitle");
    }

    #[test]
    fn a_value_that_no_longer_decodes_as_its_type_is_flagged() {
        // The schema now declares `count: int`, but the store holds bytes that are
        // not a canonical int form for that path — the field's type changed under
        // existing data.
        let source = "\
module shelf

resource Book at ^books(id: int)
    required count: int
";
        let (project, program, path) = build_project(source);
        let count_path = encode_path(&[
            PathSegment::Root("books".to_string()),
            PathSegment::RecordKey(SavedKey::Int(1)),
            PathSegment::Field("count".to_string()),
        ]);
        // Arbitrary text that is not a canonical encoded int.
        write_entries(&path, &[(count_path, b"not-an-int".to_vec())]);
        let result = impact(&project, &program);
        assert_eq!(result.findings.len(), 1, "{result:?}");
        let finding = &result.findings[0];
        assert_eq!(finding.kind, FindingKind::Undecodable);
        assert_eq!(finding.path, "^books(1).count");
        assert_eq!(finding.r#type.as_deref(), Some("int"));
        assert!(finding.message.contains("int"));
    }

    #[test]
    fn the_scan_caps_and_reports_truncation() {
        let (project, program, path) = project_with_books();
        // Seed more clean entries than the scan limit; the scan stops at the cap and
        // reports the rest as truncated.
        let entries: Vec<(Vec<u8>, Vec<u8>)> = (1..=20)
            .map(|id| (book_title(id), b"Title".to_vec()))
            .collect();
        write_entries(&path, &entries);
        let reader = StoreReader::for_project(&project).unwrap();
        // A tiny limit forces truncation deterministically without seeding 5000 rows.
        let result = schema_impact(&reader, &program, 5)
            .into_option()
            .expect("readable");
        assert_eq!(result.scanned, 5, "the scan stops at the limit: {result:?}");
        assert!(result.truncated, "more entries remained past the cap");
    }

    #[test]
    fn the_scan_limit_is_clamped_to_the_cap() {
        // A caller asking for more than the cap still scans at most the cap; with a
        // small store the page is the whole store and is not truncated.
        let (project, program, path) = project_with_books();
        write_entries(&path, &[(book_title(1), b"Mort".to_vec())]);
        let reader = StoreReader::for_project(&project).unwrap();
        let result = schema_impact(&reader, &program, usize::MAX)
            .into_option()
            .expect("readable");
        assert_eq!(result.scanned, 1);
        assert!(!result.truncated);
    }

    #[test]
    fn a_locked_store_is_unavailable() {
        let (project, program, path) = project_with_books();
        write_entries(&path, &[(book_title(1), b"Mort".to_vec())]);
        // A live writer holds the redb file lock; the read-only scan degrades to
        // Unavailable rather than erroring.
        let _writer = RedbStore::open(&path).expect("open a writer");
        let reader = StoreReader::for_project(&project).unwrap();
        assert_eq!(
            schema_impact(&reader, &program, SCAN_CAP),
            Availability::Unavailable
        );
    }

    #[test]
    fn a_missing_store_is_unavailable() {
        // The project pins a native store, but no file was ever created.
        let (project, program, _path) = project_with_books();
        let reader = StoreReader::for_project(&project).unwrap();
        assert_eq!(
            schema_impact(&reader, &program, SCAN_CAP),
            Availability::Unavailable
        );
    }
}
