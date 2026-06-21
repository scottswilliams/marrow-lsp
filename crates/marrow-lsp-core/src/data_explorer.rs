//! Saved-data presentation DTOs for editor and tool transports.
//!
//! Marrow owns saved-data traversal and typed key validation. This module only
//! translates transport DTOs, clamps child pages, and calls Marrow tooling APIs.

use serde::{Deserialize, Serialize};

use marrow_check::tooling::ToolingError;
use marrow_json::DataGenerationJson;
pub use marrow_json::saved_data::{
    DataChildViewJson as DataChildViewDto, DataChildViewsPageJson as DataChildViewsResult,
    DataChildrenRequestJson as DataChildrenRequest, DataKeyJson as DataKeyDto,
    DataPathErrorJson as DataPathErrorDto, DataPathSegmentJson as DataPathSegmentDto,
    DataPresenceJson as DataPresenceDto, DataReadRequestJson as DataReadRequest,
    DataReadResultJson as DataReadResult, DataStoreErrorCodeJson as DataStoreErrorCodeDto,
    DataStoreErrorJson as DataStoreErrorDto, DataViewBoundaryJson,
    MemberFlavorJson as MemberFlavorDto, ScalarTypeJson as ScalarTypeDto,
    data_view_boundary_to_json,
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
    pub store_snapshot: Option<DataGenerationJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_view_boundary: Option<DataViewBoundaryJson>,
}

/// Handle `marrow/savedRoots`. A `None` session (no native store configured) or an
/// unavailable store both answer `available: false` with no roots.
pub fn saved_roots(session: Option<&SavedDataSession>) -> SavedRootsResult {
    let Some(session) = session else {
        return SavedRootsResult {
            available: false,
            roots: Vec::new(),
            store_snapshot: None,
            data_view_boundary: None,
        };
    };
    match session.surface_read().saved_data_roots() {
        Ok(stamped) => SavedRootsResult {
            available: true,
            roots: stamped
                .data
                .into_iter()
                .map(DataChildViewDto::from)
                .collect(),
            store_snapshot: Some(DataGenerationJson::from(&stamped.stamp)),
            data_view_boundary: Some(data_view_boundary_to_json(session.surface_read())),
        },
        Err(_) => SavedRootsResult {
            available: false,
            roots: Vec::new(),
            store_snapshot: None,
            data_view_boundary: None,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataChildViewsResponse {
    pub available: bool,
    pub children: Vec<DataChildViewDto>,
    pub truncated: bool,
    pub cursor: Option<DataKeyDto>,
    pub store_snapshot: Option<DataGenerationJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_view_boundary: Option<DataViewBoundaryJson>,
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
    pub store_snapshot: Option<DataGenerationJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_view_boundary: Option<DataViewBoundaryJson>,
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
    Ok(DataChildViewsResult::from(stamped).with_data_view_boundary(session.data_view_boundary()))
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
            data_view_boundary: Some(data_view_boundary_to_json(session.surface_read())),
        });
    };
    Ok(DataReadResult::from(stamped).with_data_view_boundary(session.data_view_boundary()))
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
    use marrow_json::{
        DATA_GENERATION_PROFILE_VERSION, DataGenerationJson, saved_data::DataViewBoundaryJson,
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
                segments: vec![DataPathSegmentDto::Root {
                    store_catalog_id: "cat_00000000000000000000000000000001".into(),
                }],
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
                segments: vec![DataPathSegmentDto::Root {
                    store_catalog_id: "cat_00000000000000000000000000000001".into(),
                }],
                preview_limit: Some(0),
            }),
            Err(DataRequestValidationError::ZeroPreviewLimit)
        );
        assert!(
            serde_json::from_value::<DataChildrenRequest>(serde_json::json!({
                "segments": [{ "kind": "root", "value": "counter" }],
                "limit": 1,
                "cursor": null,
            }))
            .is_err(),
            "downstream saved-data requests must not accept source-spelling root authority"
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
                data_view_boundary: None,
            }
        );
    }

    #[test]
    fn saved_roots_returns_generation_profile() {
        let fixture = counter_fixture(CounterShape::default().with_ids(1..=1));
        let roots = saved_roots(Some(&fixture.session));

        assert!(roots.available);
        assert_eq!(
            roots.roots,
            vec![DataChildViewDto {
                segment: fixture.root_segment(),
                label: "counter".into(),
            }]
        );
        assert_eq!(
            serde_json::to_value(&roots.roots[0].segment).unwrap(),
            serde_json::json!({
                "kind": "root",
                "store_catalog_id": fixture.store_catalog_id,
            })
        );
        assert_generation_profile(&roots.store_snapshot);
        assert_data_view_boundary(&roots.data_view_boundary);
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
            session_data_child_views_page(
                &fixture.session,
                DataChildrenRequest {
                    segments: vec![fixture.root_segment()],
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
            session_data_child_views_page(
                &fixture.session,
                DataChildrenRequest {
                    segments: vec![fixture.root_segment(), key_segment(1),],
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
        let page = session_data_child_views_page(
            &fixture.session,
            DataChildrenRequest {
                segments: vec![fixture.root_segment()],
                limit: DATA_CHILDREN_PAGE_LIMIT + 1,
                cursor: None,
            },
        )
        .unwrap();

        assert_eq!(page.children.len(), DATA_CHILDREN_PAGE_LIMIT);
        assert_eq!(page.children.first(), Some(&key_child(1)));
        assert_eq!(page.children.last(), Some(&key_child(200)));
        assert!(page.truncated);
        assert_eq!(page.cursor, Some(DataKeyDto::Int(200)));
    }

    #[test]
    fn data_children_returns_paged_typed_segments() {
        let fixture = counter_fixture(CounterShape::default().with_ids(1..=5));
        let root = vec![fixture.root_segment()];

        let page = session_data_child_views_page(
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
            DataChildViewsResult {
                children: vec![key_child(1), key_child(2),],
                truncated: true,
                cursor: Some(DataKeyDto::Int(2)),
                store_snapshot: page.store_snapshot.clone(),
                data_view_boundary: page.data_view_boundary.clone(),
            }
        );
        assert_generation_profile(&page.store_snapshot);
        assert_data_view_boundary(&page.data_view_boundary);

        let page = session_data_child_views_page(
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
            DataChildViewsResult {
                children: vec![key_child(3), key_child(4),],
                truncated: true,
                cursor: Some(DataKeyDto::Int(4)),
                store_snapshot: page.store_snapshot.clone(),
                data_view_boundary: page.data_view_boundary.clone(),
            }
        );
        assert_generation_profile(&page.store_snapshot);
        assert_data_view_boundary(&page.data_view_boundary);

        let page = session_data_child_views_page(
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
            DataChildViewsResult {
                children: vec![key_child(5)],
                truncated: false,
                cursor: None,
                store_snapshot: page.store_snapshot.clone(),
                data_view_boundary: page.data_view_boundary.clone(),
            }
        );
        assert_generation_profile(&page.store_snapshot);
        assert_data_view_boundary(&page.data_view_boundary);
    }

    #[test]
    fn data_child_views_render_key_labels_from_marrow() {
        let fixture = counter_fixture(CounterShape::default().with_ids(1..=1));

        let page = session_data_child_views_page(
            &fixture.session,
            DataChildrenRequest {
                segments: vec![fixture.root_segment()],
                limit: 10,
                cursor: None,
            },
        )
        .unwrap();

        assert_eq!(
            page.children,
            vec![DataChildViewDto {
                segment: key_segment(1),
                label: "(1)".into(),
            }]
        );
        assert_generation_profile(&page.store_snapshot);
    }

    #[test]
    fn data_read_renders_a_present_value_leaf() {
        let fixture =
            counter_fixture(CounterShape::default().with_values([(1, SavedValue::Int(42))]));

        let read = session_data_read(
            &fixture.session,
            DataReadRequest {
                segments: vec![
                    fixture.root_segment(),
                    key_segment(1),
                    fixture.value_field_segment(),
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
                data_view_boundary: read.data_view_boundary.clone(),
            }
        );
        assert_generation_profile(&read.store_snapshot);
        assert_data_view_boundary(&read.data_view_boundary);
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
                    fixture.root_segment(),
                    key_segment(1),
                    fixture.value_field_segment(),
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
                data_view_boundary: read.data_view_boundary.clone(),
            }
        );
        assert_generation_profile(&read.store_snapshot);
        assert_data_view_boundary(&read.data_view_boundary);
    }

    fn assert_data_view_boundary(boundary: &Option<DataViewBoundaryJson>) {
        let boundary = serde_json::to_value(
            boundary
                .as_ref()
                .expect("available data response carries boundary"),
        )
        .expect("boundary serializes");
        assert_eq!(
            boundary["sourceAnalysisGeneration"]["profileVersion"], "analysis.generation.v1",
            "{boundary}"
        );
        assert_eq!(
            boundary["storeSnapshot"]["profile_version"], DATA_GENERATION_PROFILE_VERSION,
            "{boundary}"
        );
        assert_eq!(
            boundary["compatibility"],
            serde_json::json!({ "verdict": "admitted" }),
            "{boundary}"
        );
        assert!(
            boundary["watchTargets"]
                .as_array()
                .is_some_and(|targets| targets.len() == 2),
            "{boundary}"
        );
    }

    fn assert_generation_profile(snapshot: &Option<DataGenerationJson>) {
        let snapshot = snapshot.as_ref().expect("response carries generation");
        assert_eq!(snapshot.profile_version, DATA_GENERATION_PROFILE_VERSION);
    }

    fn key_segment(value: i64) -> DataPathSegmentDto {
        DataPathSegmentDto::Key {
            value: DataKeyDto::Int(value),
        }
    }

    fn key_child(value: i64) -> DataChildViewDto {
        DataChildViewDto {
            segment: key_segment(value),
            label: format!("({value})"),
        }
    }

    struct CounterFixture {
        _dir: tempfile::TempDir,
        session: crate::store::SavedDataSession,
        store_catalog_id: String,
        value_member_catalog_id: String,
    }

    impl CounterFixture {
        fn root_segment(&self) -> DataPathSegmentDto {
            DataPathSegmentDto::Root {
                store_catalog_id: self.store_catalog_id.clone(),
            }
        }

        fn value_field_segment(&self) -> DataPathSegmentDto {
            DataPathSegmentDto::Field {
                member_catalog_id: self.value_member_catalog_id.clone(),
            }
        }
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
        let store_catalog_id = store_id.as_str().to_string();
        let value_member_catalog_id = value_id.clone();
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
        CounterFixture {
            _dir: dir,
            session,
            store_catalog_id,
            value_member_catalog_id,
        }
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
