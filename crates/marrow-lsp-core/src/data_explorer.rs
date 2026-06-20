//! Saved-data presentation DTOs for editor and tool transports.
//!
//! Marrow owns saved-data traversal and typed key validation. This module only
//! translates transport DTOs, clamps child pages, and calls Marrow tooling APIs.

use serde::{Deserialize, Serialize};

use marrow_check::CheckedProgram;
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
use marrow_store::tree::TreeStore;

use crate::store::{Availability, LiveStore};

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

/// Handle `marrow/savedRoots`. A `None` reader (no native store configured) or an
/// unavailable store both answer `available: false` with no roots.
pub fn saved_roots(reader: Option<&LiveStore>, program: &CheckedProgram) -> SavedRootsResult {
    match reader.map(|reader| reader.root_views(program)) {
        Some(Availability::Available(stamped)) => SavedRootsResult {
            available: true,
            roots: stamped.data,
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

pub fn data_children_page(
    program: &CheckedProgram,
    store: &TreeStore,
    request: DataChildrenRequest,
) -> Result<DataChildrenResult, DataChildrenError> {
    let (segments, limit, cursor) = request.into_path_parts();
    let stamped = tooling::stamped_data_children(
        program,
        store,
        &segments,
        limit.min(DATA_CHILDREN_PAGE_LIMIT),
        cursor.as_ref(),
    )?;
    Ok(DataChildrenResult::from(stamped))
}

pub fn data_child_views_page(
    program: &CheckedProgram,
    store: &TreeStore,
    request: DataChildrenRequest,
) -> Result<DataChildViewsResult, DataChildrenError> {
    let (segments, limit, cursor) = request.into_path_parts();
    let stamped = tooling::stamped_data_children(
        program,
        store,
        &segments,
        limit.min(DATA_CHILDREN_PAGE_LIMIT),
        cursor.as_ref(),
    )?;
    Ok(DataChildViewsResult::from(stamped))
}

pub fn data_read(
    program: &CheckedProgram,
    store: &TreeStore,
    request: DataReadRequest,
) -> Result<DataReadResult, DataChildrenError> {
    let preview_limit = request.preview_limit_or_default();
    let segments = request.into_path_segments();
    let Some(resolved_path) = tooling::resolve_data_path(program, &segments)? else {
        return Ok(DataReadResult {
            presence: DataPresenceDto::Absent,
            value: None,
            value_truncated: false,
            store_snapshot: None,
        });
    };
    let stamped = tooling::stamped_preview_data_path(program, store, &resolved_path, preview_limit)
        .map_err(|error| DataChildrenError::Store(DataStoreErrorDto::from(error)))?;
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

    use marrow_check::test_support::{
        commit_then_check, member_catalog_id, root_place, store_id_of,
    };
    use marrow_store::key::SavedKey;
    use marrow_store::tree::{DataPathSegment, TreeStore};
    use marrow_store::value::{SavedValue, encode_value};

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
    fn a_missing_reader_answers_unavailable() {
        let program = CheckedProgram::default();
        assert_eq!(
            saved_roots(None, &program),
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
        let (program, store) = checked_counter_store(1..=1);
        assert_eq!(
            data_children_page(
                &program,
                &store,
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
        let (program, store) =
            checked_counter_store_with_keys("string", std::iter::empty::<SavedKey>());
        assert_eq!(
            data_children_page(
                &program,
                &store,
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
        let (program, store) = checked_counter_store(1..=201);
        let page = data_children_page(
            &program,
            &store,
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
        let (program, store) = checked_counter_store(1..=5);
        let root = vec![DataPathSegmentDto::Root("counter".into())];

        let page = data_children_page(
            &program,
            &store,
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
                store_snapshot: Some(marrow_json::DataSnapshotJson {
                    store_uid: None,
                    catalog_digest: None,
                    commit: None,
                    checked_source_digest: program.source_digest(),
                }),
            }
        );

        let page = data_children_page(
            &program,
            &store,
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
                store_snapshot: Some(marrow_json::DataSnapshotJson {
                    store_uid: None,
                    catalog_digest: None,
                    commit: None,
                    checked_source_digest: program.source_digest(),
                }),
            }
        );

        let page = data_children_page(
            &program,
            &store,
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
                store_snapshot: Some(marrow_json::DataSnapshotJson {
                    store_uid: None,
                    catalog_digest: None,
                    commit: None,
                    checked_source_digest: program.source_digest(),
                }),
            }
        );
    }

    #[test]
    fn data_child_views_render_key_labels_from_marrow() {
        let (program, store) = checked_counter_store(1..=1);

        let page = data_child_views_page(
            &program,
            &store,
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
        assert_eq!(
            page.store_snapshot,
            Some(marrow_json::DataSnapshotJson {
                store_uid: None,
                catalog_digest: None,
                commit: None,
                checked_source_digest: program.source_digest(),
            })
        );
    }

    #[test]
    fn data_read_renders_a_present_value_leaf() {
        let (program, store) = checked_counter_store_with_values([(1, 42)]);

        let read = data_read(
            &program,
            &store,
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
                store_snapshot: Some(marrow_json::DataSnapshotJson {
                    store_uid: None,
                    catalog_digest: None,
                    commit: None,
                    checked_source_digest: program.source_digest(),
                }),
            }
        );
    }

    #[test]
    fn data_read_uses_marrow_bounded_preview_contract() {
        let long = "a".repeat(64);
        let (program, store) =
            checked_counter_store_with_saved_values("string", [(1, SavedValue::Str(long))]);

        let read = data_read(
            &program,
            &store,
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
                store_snapshot: Some(marrow_json::DataSnapshotJson {
                    store_uid: None,
                    catalog_digest: None,
                    commit: None,
                    checked_source_digest: program.source_digest(),
                }),
            }
        );
    }

    fn checked_counter_store(ids: std::ops::RangeInclusive<i64>) -> (CheckedProgram, TreeStore) {
        checked_counter_store_with_keys("int", ids.map(SavedKey::Int))
    }

    fn checked_counter_store_with_values(
        values: impl IntoIterator<Item = (i64, i64)>,
    ) -> (CheckedProgram, TreeStore) {
        checked_counter_store_with_saved_values(
            "int",
            values
                .into_iter()
                .map(|(identity, value)| (identity, SavedValue::Int(value))),
        )
    }

    fn checked_counter_store_with_saved_values(
        value_type: &str,
        values: impl IntoIterator<Item = (i64, SavedValue)>,
    ) -> (CheckedProgram, TreeStore) {
        let (program, store) =
            checked_counter_store_with_shape("int", value_type, std::iter::empty());
        let place = root_place(&program, "counter").unwrap();
        let store_id = store_id_of(&place).unwrap();
        let value_id = member_catalog_id(&place, "value").unwrap();
        let path = vec![DataPathSegment::Member(
            marrow_store::cell::CatalogId::new(value_id).unwrap(),
        )];
        for (identity, value) in values {
            let identity = [SavedKey::Int(identity)];
            store.write_record_presence(&store_id, &identity).unwrap();
            store
                .write_data_value(&store_id, &identity, &path, encode_value(&value).unwrap())
                .unwrap();
        }
        (program, store)
    }

    fn checked_counter_store_with_keys(
        key_type: &str,
        keys: impl IntoIterator<Item = SavedKey>,
    ) -> (CheckedProgram, TreeStore) {
        checked_counter_store_with_shape(key_type, "int", keys)
    }

    fn checked_counter_store_with_shape(
        key_type: &str,
        value_type: &str,
        keys: impl IntoIterator<Item = SavedKey>,
    ) -> (CheckedProgram, TreeStore) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("app.mw"),
            format!(
                r#"
module app

resource Counter
    required value: {value_type}

store ^counter(id: {key_type}): Counter
"#
            ),
        )
        .unwrap();

        let program = commit_then_check(dir.path()).unwrap();
        let place = root_place(&program, "counter").unwrap();
        let store_id = store_id_of(&place).unwrap();
        let store = TreeStore::memory();
        for key in keys {
            store.write_record_presence(&store_id, &[key]).unwrap();
        }
        (program, store)
    }
}
