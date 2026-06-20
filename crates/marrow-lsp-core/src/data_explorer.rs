//! Saved-data presentation DTOs for editor and tool transports.
//!
//! Marrow owns saved-data traversal and typed key validation. This module only
//! translates transport DTOs, clamps child pages, and calls Marrow tooling APIs.

use serde::{Deserialize, Serialize};

use marrow_check::tooling::{self, ToolingError};
use marrow_json::DataSnapshotJson;
pub use marrow_json::saved_data::{
    DataChildJson as DataChildDto, DataChildViewJson as DataChildViewDto,
    DataChildViewsPageJson as DataChildViewsResult, DataChildrenPageJson as DataChildrenResult,
    DataChildrenRequestJson as DataChildrenRequest, DataKeyJson as DataKeyDto,
    DataPathErrorJson as DataPathErrorDto, DataPathSegmentJson as DataPathSegmentDto,
    DataPresenceJson as DataPresenceDto, DataReadRequestJson as DataReadRequest,
    DataReadResultJson as DataReadResult, DataStoreErrorCodeJson as DataStoreErrorCodeDto,
    DataStoreErrorJson as DataStoreErrorDto, MemberFlavorJson as MemberFlavorDto,
    ScalarTypeJson as ScalarTypeDto,
};

use crate::store::SavedDataSession;

pub const DATA_CHILDREN_PAGE_LIMIT: usize = 200;

/// `marrow/savedRoots` reply: the saved root views, plus whether the store could
/// be read. When `available` is false the roots are empty and the tree view shows
/// an unavailable state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SavedRootsResult {
    pub available: bool,
    pub roots: Vec<DataChildViewDto>,
    pub store_snapshot: Option<DataSnapshotJson>,
}

/// Handle `marrow/savedRoots`. A `None` session (no native store configured) or an
/// unavailable store both answer `available: false` with no roots.
pub fn saved_roots(session: Option<&SavedDataSession>) -> SavedRootsResult {
    match session.map(|session| session.surface_read().saved_data_roots()) {
        Some(Ok(stamped)) => SavedRootsResult {
            available: true,
            roots: stamped
                .data
                .into_iter()
                .map(|root| DataChildViewDto::from(tooling::DataChild::Root(root)))
                .collect(),
            store_snapshot: Some(DataSnapshotJson::from(&stamped.stamp)),
        },
        _ => SavedRootsResult {
            available: false,
            roots: Vec::new(),
            store_snapshot: None,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataChildViewsResponse {
    pub available: bool,
    pub children: Vec<DataChildViewDto>,
    pub truncated: bool,
    pub cursor: Option<DataKeyDto>,
    pub store_snapshot: Option<DataSnapshotJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DataChildrenError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataReadResponse {
    pub available: bool,
    pub presence: DataPresenceDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub value_truncated: bool,
    pub store_snapshot: Option<DataSnapshotJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DataChildrenError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataRequestValidationError {
    EmptySegments,
    ZeroLimit,
    ZeroPreviewLimit,
}

impl DataRequestValidationError {
    pub fn message(self) -> &'static str {
        match self {
            Self::EmptySegments => "`segments` must not be empty",
            Self::ZeroLimit => "`limit` must be at least 1",
            Self::ZeroPreviewLimit => "`preview_limit` must be at least 1",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DataChildrenError {
    Path(DataPathErrorDto),
    Store(DataStoreErrorDto),
}

pub fn validate_data_children_request(
    request: DataChildrenRequest,
) -> Result<DataChildrenRequest, DataRequestValidationError> {
    if request.segments.is_empty() {
        return Err(DataRequestValidationError::EmptySegments);
    }
    if request.limit == 0 {
        return Err(DataRequestValidationError::ZeroLimit);
    }
    Ok(request)
}

pub fn validate_data_read_request(
    request: DataReadRequest,
) -> Result<DataReadRequest, DataRequestValidationError> {
    if request.segments.is_empty() {
        return Err(DataRequestValidationError::EmptySegments);
    }
    if request.preview_limit == Some(0) {
        return Err(DataRequestValidationError::ZeroPreviewLimit);
    }
    Ok(request)
}

pub fn session_data_children_page(
    session: &SavedDataSession,
    request: DataChildrenRequest,
) -> Result<DataChildrenResult, DataChildrenError> {
    let (segments, limit, cursor) = request.into_path_parts();
    let stamped = session.surface_read().saved_data_children(
        &segments,
        limit.min(DATA_CHILDREN_PAGE_LIMIT),
        cursor.as_ref(),
    )?;
    Ok(DataChildrenResult::from(stamped))
}

pub fn session_data_child_views_page(
    session: &SavedDataSession,
    request: DataChildrenRequest,
) -> Result<DataChildViewsResult, DataChildrenError> {
    let (segments, limit, cursor) = request.into_path_parts();
    let stamped = session.surface_read().saved_data_children(
        &segments,
        limit.min(DATA_CHILDREN_PAGE_LIMIT),
        cursor.as_ref(),
    )?;
    Ok(DataChildViewsResult::from(stamped))
}

pub fn session_data_read(
    session: &SavedDataSession,
    request: DataReadRequest,
) -> Result<DataReadResult, DataChildrenError> {
    let preview_limit = request.preview_limit_or_default();
    let segments = request.into_path_segments();
    let Some(stamped) = session
        .surface_read()
        .saved_data_preview(&segments, preview_limit)?
    else {
        return Ok(DataReadResult {
            presence: DataPresenceDto::Absent,
            value: None,
            value_truncated: false,
            store_snapshot: None,
        });
    };
    Ok(DataReadResult::from(stamped))
}

impl From<ToolingError> for DataChildrenError {
    fn from(error: ToolingError) -> Self {
        match error {
            ToolingError::Path(error) => Self::Path(DataPathErrorDto::from(error)),
            ToolingError::Store(error) => Self::Store(DataStoreErrorDto::from(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use marrow_check::test_support::{member_catalog_id, root_place, store_id_of};
    use marrow_check::{
        check_project, check_project_against, project_store_lock, read_committed_lock,
    };
    use marrow_project::parse_config;
    use marrow_store::key::SavedKey;
    use marrow_store::tree::{DataPathSegment, StoreUid, TreeStore};
    use marrow_store::value::{SavedValue, encode_value};

    use crate::store::open_saved_data_session;
    use crate::workspace::Project;

    #[test]
    fn data_request_validation_rejects_empty_paths_and_zero_limits() {
        assert_eq!(
            validate_data_children_request(DataChildrenRequest {
                segments: Vec::new(),
                limit: 1,
                cursor: None,
            }),
            Err(DataRequestValidationError::EmptySegments)
        );
        assert_eq!(
            validate_data_children_request(DataChildrenRequest {
                segments: vec![DataPathSegmentDto::Root("counter".into())],
                limit: 0,
                cursor: None,
            }),
            Err(DataRequestValidationError::ZeroLimit)
        );
        assert_eq!(
            validate_data_read_request(DataReadRequest {
                segments: Vec::new(),
                preview_limit: None,
            }),
            Err(DataRequestValidationError::EmptySegments)
        );
        assert_eq!(
            validate_data_read_request(DataReadRequest {
                segments: vec![DataPathSegmentDto::Root("counter".into())],
                preview_limit: Some(0),
            }),
            Err(DataRequestValidationError::ZeroPreviewLimit)
        );
    }

    #[test]
    fn a_missing_session_answers_unavailable() {
        assert_eq!(
            saved_roots(None),
            SavedRootsResult {
                available: false,
                roots: Vec::new(),
                store_snapshot: None,
            }
        );
    }

    #[test]
    fn store_errors_report_typed_codes() {
        let error = DataChildrenError::from(marrow_check::tooling::ToolingError::Store(
            marrow_store::StoreError::FormatVersion {
                found: 1,
                supported: 2,
            },
        ));
        let DataChildrenError::Store(error) = error else {
            panic!("expected a store error");
        };
        assert_eq!(error.code, DataStoreErrorCodeDto::FormatVersion);
        assert!(!error.message.is_empty());
    }

    #[test]
    fn zero_limit_returns_path_error() {
        let fixture = counter_fixture(CounterShape::default().with_ids(1..=1));
        assert_eq!(
            session_data_children_page(
                &fixture.session,
                DataChildrenRequest {
                    segments: vec![DataPathSegmentDto::Root("counter".into())],
                    limit: 0,
                    cursor: None,
                },
            ),
            Err(DataChildrenError::Path(DataPathErrorDto::ZeroLimit))
        );
    }

    #[test]
    fn typed_path_error_reports_string_scalar_type() {
        let fixture = counter_fixture(CounterShape::default().with_key_type("string"));
        assert_eq!(
            session_data_children_page(
                &fixture.session,
                DataChildrenRequest {
                    segments: vec![
                        DataPathSegmentDto::Root("counter".into()),
                        DataPathSegmentDto::Key(DataKeyDto::Int(1)),
                    ],
                    limit: 2,
                    cursor: None,
                },
            ),
            Err(DataChildrenError::Path(DataPathErrorDto::IdentityKeyType {
                root: "counter".into(),
                expected: ScalarTypeDto::String,
                found: ScalarTypeDto::Int,
            }))
        );
    }

    #[test]
    fn data_children_clamps_large_caller_limit() {
        let fixture = counter_fixture(CounterShape::default().with_ids(1..=201));
        let page = session_data_children_page(
            &fixture.session,
            DataChildrenRequest {
                segments: vec![DataPathSegmentDto::Root("counter".into())],
                limit: DATA_CHILDREN_PAGE_LIMIT + 1,
                cursor: None,
            },
        )
        .unwrap();

        assert_eq!(page.children.len(), DATA_CHILDREN_PAGE_LIMIT);
        assert_eq!(
            page.children.first(),
            Some(&DataChildDto::Key(DataKeyDto::Int(1)))
        );
        assert_eq!(
            page.children.last(),
            Some(&DataChildDto::Key(DataKeyDto::Int(200)))
        );
        assert!(page.truncated);
        assert_eq!(page.cursor, Some(DataKeyDto::Int(200)));
    }

    #[test]
    fn data_children_returns_paged_typed_segments() {
        let fixture = counter_fixture(CounterShape::default().with_ids(1..=5));
        let root = vec![DataPathSegmentDto::Root("counter".into())];

        let page = session_data_children_page(
            &fixture.session,
            DataChildrenRequest {
                segments: root.clone(),
                limit: 2,
                cursor: None,
            },
        )
        .unwrap();
        assert_eq!(
            page,
            DataChildrenResult {
                children: vec![
                    DataChildDto::Key(DataKeyDto::Int(1)),
                    DataChildDto::Key(DataKeyDto::Int(2)),
                ],
                truncated: true,
                cursor: Some(DataKeyDto::Int(2)),
                store_snapshot: page.store_snapshot.clone(),
            }
        );
        assert!(page.store_snapshot.is_some());

        let page = session_data_children_page(
            &fixture.session,
            DataChildrenRequest {
                segments: root.clone(),
                limit: 2,
                cursor: Some(DataKeyDto::Int(2)),
            },
        )
        .unwrap();
        assert_eq!(
            page,
            DataChildrenResult {
                children: vec![
                    DataChildDto::Key(DataKeyDto::Int(3)),
                    DataChildDto::Key(DataKeyDto::Int(4)),
                ],
                truncated: true,
                cursor: Some(DataKeyDto::Int(4)),
                store_snapshot: page.store_snapshot.clone(),
            }
        );
        assert!(page.store_snapshot.is_some());

        let page = session_data_children_page(
            &fixture.session,
            DataChildrenRequest {
                segments: root,
                limit: 2,
                cursor: Some(DataKeyDto::Int(4)),
            },
        )
        .unwrap();
        assert_eq!(
            page,
            DataChildrenResult {
                children: vec![DataChildDto::Key(DataKeyDto::Int(5))],
                truncated: false,
                cursor: None,
                store_snapshot: page.store_snapshot.clone(),
            }
        );
        assert!(page.store_snapshot.is_some());
    }

    #[test]
    fn data_child_views_render_key_labels_from_marrow() {
        let fixture = counter_fixture(CounterShape::default().with_ids(1..=1));

        let page = session_data_child_views_page(
            &fixture.session,
            DataChildrenRequest {
                segments: vec![DataPathSegmentDto::Root("counter".into())],
                limit: 10,
                cursor: None,
            },
        )
        .unwrap();

        assert_eq!(
            page.children,
            vec![DataChildViewDto {
                segment: DataPathSegmentDto::Key(DataKeyDto::Int(1)),
                label: "(1)".into(),
            }]
        );
        assert!(page.store_snapshot.is_some());
    }

    #[test]
    fn data_read_renders_a_present_value_leaf() {
        let fixture =
            counter_fixture(CounterShape::default().with_values([(1, SavedValue::Int(42))]));

        let read = session_data_read(
            &fixture.session,
            DataReadRequest {
                segments: vec![
                    DataPathSegmentDto::Root("counter".into()),
                    DataPathSegmentDto::Key(DataKeyDto::Int(1)),
                    DataPathSegmentDto::Field("value".into()),
                ],
                preview_limit: None,
            },
        )
        .unwrap();

        assert_eq!(
            read,
            DataReadResult {
                presence: DataPresenceDto::ValueOnly,
                value: Some("42".into()),
                value_truncated: false,
                store_snapshot: read.store_snapshot.clone(),
            }
        );
        assert!(read.store_snapshot.is_some());
    }

    #[test]
    fn data_read_uses_marrow_bounded_preview_contract() {
        let long = "a".repeat(64);
        let fixture = counter_fixture(
            CounterShape::default()
                .with_value_type("string")
                .with_values([(1, SavedValue::Str(long))]),
        );

        let read = session_data_read(
            &fixture.session,
            DataReadRequest {
                segments: vec![
                    DataPathSegmentDto::Root("counter".into()),
                    DataPathSegmentDto::Key(DataKeyDto::Int(1)),
                    DataPathSegmentDto::Field("value".into()),
                ],
                preview_limit: Some(8),
            },
        )
        .unwrap();

        assert_eq!(
            read,
            DataReadResult {
                presence: DataPresenceDto::ValueOnly,
                value: Some("\"aaaaaaa...".into()),
                value_truncated: true,
                store_snapshot: read.store_snapshot.clone(),
            }
        );
        assert!(read.store_snapshot.is_some());
    }

    struct CounterFixture {
        _dir: tempfile::TempDir,
        session: crate::store::SavedDataSession,
    }

    struct CounterShape {
        key_type: &'static str,
        value_type: &'static str,
        ids: Vec<i64>,
        values: Vec<(i64, SavedValue)>,
    }

    impl CounterShape {
        fn with_key_type(mut self, key_type: &'static str) -> Self {
            self.key_type = key_type;
            self
        }

        fn with_value_type(mut self, value_type: &'static str) -> Self {
            self.value_type = value_type;
            self
        }

        fn with_ids(mut self, ids: std::ops::RangeInclusive<i64>) -> Self {
            self.ids = ids.collect();
            self
        }

        fn with_values(mut self, values: impl IntoIterator<Item = (i64, SavedValue)>) -> Self {
            self.values = values.into_iter().collect();
            self
        }
    }

    fn counter_fixture(mut shape: CounterShape) -> CounterFixture {
        if shape.key_type.is_empty() {
            shape.key_type = "int";
        }
        if shape.value_type.is_empty() {
            shape.value_type = "int";
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("app.mw"),
            format!(
                r#"
module app

resource Counter
    required value: {}

store ^counter(id: {}): Counter
"#,
                shape.value_type, shape.key_type
            ),
        )
        .unwrap();

        let config = parse_config(&fs::read_to_string(root.join("marrow.json")).unwrap()).unwrap();
        let (report, proposed) = check_project(root, &config).unwrap();
        assert!(!report.has_errors(), "{:#?}", report.diagnostics);
        let proposal = proposed.catalog.proposal.as_ref().unwrap();
        project_store_lock(root, proposal, &proposed.source_digest()).unwrap();
        let lock = read_committed_lock(root).unwrap();
        let program = check_project_against(root, &config, None, lock.as_ref()).unwrap();
        let place = root_place(&program, "counter").unwrap();
        let store_id = store_id_of(&place).unwrap();
        let value_id = member_catalog_id(&place, "value").unwrap();
        let path = vec![DataPathSegment::Member(
            marrow_store::cell::CatalogId::new(value_id).unwrap(),
        )];
        let data_dir = root.join("data");
        fs::create_dir_all(&data_dir).unwrap();
        let store = TreeStore::open(&data_dir.join("marrow.redb")).unwrap();
        store
            .write_store_uid(&StoreUid::new("store_00000000000000000000000000000001").unwrap())
            .unwrap();
        marrow_run::evolution::commit_catalog_baseline(&store, &program).unwrap();
        for id in shape.ids {
            store
                .write_record_presence(&store_id, &[SavedKey::Int(id)])
                .unwrap();
        }
        for (identity, value) in shape.values {
            let identity = [SavedKey::Int(identity)];
            store.write_record_presence(&store_id, &identity).unwrap();
            store
                .write_data_value(&store_id, &identity, &path, encode_value(&value).unwrap())
                .unwrap();
        }
        drop(store);
        let session = open_saved_data_session(&Project {
            root: root.to_path_buf(),
            config,
        })
        .unwrap()
        .unwrap();
        CounterFixture { _dir: dir, session }
    }

    impl Default for CounterShape {
        fn default() -> Self {
            Self {
                key_type: "int",
                value_type: "int",
                ids: Vec::new(),
                values: Vec::new(),
            }
        }
    }
}
