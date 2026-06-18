//! Saved-data presentation DTOs for editor and tool transports.
//!
//! Marrow owns saved-data traversal and typed key validation. This module only
//! translates transport DTOs, clamps child pages, and calls Marrow tooling APIs.

use serde::{Deserialize, Serialize};

use marrow_check::CheckedProgram;
use marrow_check::tooling::{
    self, DataChild, DataPresence, DataQuerySegment, MemberFlavor, QueryError, ToolingError,
};
use marrow_store::StoreError;
use marrow_store::key::SavedKey;
use marrow_store::tree::TreeStore;

use crate::store::{Availability, LiveStore};

pub const DATA_CHILDREN_PAGE_LIMIT: usize = 200;
pub const DATA_VALUE_PREVIEW_LIMIT: usize = 2048;

/// `marrow/savedRoots` reply: the saved root names, plus whether the store could
/// be read. When `available` is false the roots are empty and the tree view shows
/// an unavailable state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRootsResult {
    pub available: bool,
    pub roots: Vec<String>,
}

/// Handle `marrow/savedRoots`. A `None` reader (no native store configured) or an
/// unavailable store both answer `available: false` with no roots.
pub fn saved_roots(reader: Option<&LiveStore>, program: &CheckedProgram) -> SavedRootsResult {
    match reader.map(|reader| reader.roots(program)) {
        Some(Availability::Available(roots)) => SavedRootsResult {
            available: true,
            roots,
        },
        _ => SavedRootsResult {
            available: false,
            roots: Vec::new(),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DataPathSegmentDto {
    Root(String),
    Field(String),
    Layer(String),
    Key(DataKeyDto),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DataKeyDto {
    Int(i64),
    Bool(bool),
    String(String),
    Date(i32),
    Instant(#[serde(with = "i128_string")] i128),
    Duration(#[serde(with = "i128_string")] i128),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DataChildDto {
    Root(String),
    Key(DataKeyDto),
    Field(String),
    Layer(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataChildrenRequest {
    pub segments: Vec<DataPathSegmentDto>,
    pub limit: usize,
    pub cursor: Option<DataKeyDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataChildrenResult {
    pub children: Vec<DataChildDto>,
    pub truncated: bool,
    pub cursor: Option<DataKeyDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataChildViewDto {
    pub segment: DataPathSegmentDto,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataChildViewsResult {
    pub children: Vec<DataChildViewDto>,
    pub truncated: bool,
    pub cursor: Option<DataKeyDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataChildViewsResponse {
    pub available: bool,
    pub children: Vec<DataChildViewDto>,
    pub truncated: bool,
    pub cursor: Option<DataKeyDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DataChildrenError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataReadRequest {
    pub segments: Vec<DataPathSegmentDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataReadResult {
    pub presence: DataPresenceDto,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataReadResponse {
    pub available: bool,
    pub presence: DataPresenceDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DataChildrenError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataPresenceDto {
    Absent,
    ValueOnly,
    ChildrenOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DataChildrenError {
    Path(DataPathErrorDto),
    Store(DataStoreErrorDto),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataStoreErrorDto {
    pub code: DataStoreErrorCodeDto,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataStoreErrorCodeDto {
    Io,
    Locked,
    FormatVersion,
    Corruption,
    RecoveryRequired,
    LimitExceeded,
    InvalidCursor,
    InvalidTransaction,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum DataPathErrorDto {
    MissingRoot,
    UnknownRoot {
        root: String,
    },
    TooManyIdentityKeys {
        root: String,
    },
    IdentityKeyType {
        root: String,
        expected: ScalarTypeDto,
        found: ScalarTypeDto,
    },
    MissingIdentityKeys {
        root: String,
        expected: usize,
    },
    UnexpectedKey,
    UnknownMember {
        flavor: MemberFlavorDto,
        name: String,
    },
    TooManyMemberKeys {
        member: String,
    },
    MemberKeyType {
        member: String,
        expected: ScalarTypeDto,
        found: ScalarTypeDto,
    },
    IncompleteMemberKeys {
        member: String,
    },
    ZeroLimit,
    CursorOutsidePath,
    CursorNotAPosition,
    CursorNotAnEntry,
    MembersTakeNoCursor,
    NoChildScan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScalarTypeDto {
    Bool,
    Int,
    String,
    Bytes,
    Date,
    Duration,
    Instant,
    Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberFlavorDto {
    Field,
    Layer,
    Member,
}

pub fn data_children_page(
    program: &CheckedProgram,
    store: &TreeStore,
    request: DataChildrenRequest,
) -> Result<DataChildrenResult, DataChildrenError> {
    let (segments, limit, cursor) = data_children_parts(request);
    let page = tooling::data_children(
        program,
        store,
        &segments,
        limit.min(DATA_CHILDREN_PAGE_LIMIT),
        cursor.as_ref(),
    )?;
    Ok(DataChildrenResult::from(page))
}

pub fn data_child_views_page(
    program: &CheckedProgram,
    store: &TreeStore,
    request: DataChildrenRequest,
) -> Result<DataChildViewsResult, DataChildrenError> {
    let (segments, limit, cursor) = data_children_parts(request);
    let page = tooling::data_children(
        program,
        store,
        &segments,
        limit.min(DATA_CHILDREN_PAGE_LIMIT),
        cursor.as_ref(),
    )?;
    Ok(DataChildViewsResult::from(page))
}

pub fn data_read(
    program: &CheckedProgram,
    store: &TreeStore,
    request: DataReadRequest,
) -> Result<DataReadResult, DataChildrenError> {
    let segments = request
        .segments
        .into_iter()
        .map(DataQuerySegment::from)
        .collect::<Vec<_>>();
    let Some(query) = tooling::resolve_data_query(program, &segments)? else {
        return Ok(DataReadResult {
            presence: DataPresenceDto::Absent,
            value: None,
        });
    };
    let read = tooling::read_data_query(store, &query)
        .map_err(|error| DataChildrenError::Store(DataStoreErrorDto::from(error)))?;
    let value = read
        .payload
        .map(|payload| render_value_preview(program, &query, payload.as_bytes()));
    Ok(DataReadResult {
        presence: DataPresenceDto::from(read.presence),
        value,
    })
}

fn render_value_preview(
    program: &CheckedProgram,
    query: &tooling::DataQuery,
    bytes: &[u8],
) -> String {
    let rendered = tooling::render_data_query_value(program, query, bytes);
    truncate_preview(rendered, DATA_VALUE_PREVIEW_LIMIT)
}

fn truncate_preview(mut text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let end = text
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= limit)
        .last()
        .unwrap_or(0);
    text.truncate(end);
    text.push_str("...");
    text
}

fn data_children_parts(
    request: DataChildrenRequest,
) -> (Vec<DataQuerySegment>, usize, Option<SavedKey>) {
    let DataChildrenRequest {
        segments,
        limit,
        cursor,
    } = request;
    let segments = segments
        .into_iter()
        .map(DataQuerySegment::from)
        .collect::<Vec<_>>();
    let cursor = cursor.map(SavedKey::from);
    (segments, limit, cursor)
}

impl From<DataPathSegmentDto> for DataQuerySegment {
    fn from(segment: DataPathSegmentDto) -> Self {
        match segment {
            DataPathSegmentDto::Root(root) => Self::Root(root),
            DataPathSegmentDto::Field(field) => Self::Field(field),
            DataPathSegmentDto::Layer(layer) => Self::Layer(layer),
            DataPathSegmentDto::Key(key) => Self::Key(SavedKey::from(key)),
        }
    }
}

impl From<DataQuerySegment> for DataPathSegmentDto {
    fn from(segment: DataQuerySegment) -> Self {
        match segment {
            DataQuerySegment::Root(root) => Self::Root(root),
            DataQuerySegment::Field(field) => Self::Field(field),
            DataQuerySegment::Layer(layer) => Self::Layer(layer),
            DataQuerySegment::Key(key) => Self::Key(DataKeyDto::from(key)),
        }
    }
}

impl From<DataKeyDto> for SavedKey {
    fn from(key: DataKeyDto) -> Self {
        match key {
            DataKeyDto::Int(value) => Self::Int(value),
            DataKeyDto::Bool(value) => Self::Bool(value),
            DataKeyDto::String(value) => Self::Str(value),
            DataKeyDto::Date(value) => Self::Date(value),
            DataKeyDto::Instant(value) => Self::Instant(value),
            DataKeyDto::Duration(value) => Self::Duration(value),
            DataKeyDto::Bytes(value) => Self::Bytes(value),
        }
    }
}

impl From<SavedKey> for DataKeyDto {
    fn from(key: SavedKey) -> Self {
        match key {
            SavedKey::Int(value) => Self::Int(value),
            SavedKey::Bool(value) => Self::Bool(value),
            SavedKey::Str(value) => Self::String(value),
            SavedKey::Date(value) => Self::Date(value),
            SavedKey::Instant(value) => Self::Instant(value),
            SavedKey::Duration(value) => Self::Duration(value),
            SavedKey::Bytes(value) => Self::Bytes(value),
        }
    }
}

impl From<DataChild> for DataChildDto {
    fn from(child: DataChild) -> Self {
        match child {
            DataChild::Root(root) => Self::Root(root),
            DataChild::Key(key) => Self::Key(DataKeyDto::from(key)),
            DataChild::Field(field) => Self::Field(field),
            DataChild::Layer(layer) => Self::Layer(layer),
        }
    }
}

impl From<DataChild> for DataChildViewDto {
    fn from(child: DataChild) -> Self {
        match child {
            DataChild::Root(root) => Self {
                segment: DataPathSegmentDto::Root(root.clone()),
                label: root,
            },
            DataChild::Key(key) => {
                let label = tooling::render_query_segments(&[DataQuerySegment::Key(key.clone())]);
                Self {
                    segment: DataPathSegmentDto::Key(DataKeyDto::from(key)),
                    label,
                }
            }
            DataChild::Field(field) => Self {
                segment: DataPathSegmentDto::Field(field.clone()),
                label: field,
            },
            DataChild::Layer(layer) => Self {
                segment: DataPathSegmentDto::Layer(layer.clone()),
                label: layer,
            },
        }
    }
}

impl From<tooling::DataChildrenPage> for DataChildrenResult {
    fn from(page: tooling::DataChildrenPage) -> Self {
        Self {
            children: page.children.into_iter().map(DataChildDto::from).collect(),
            truncated: page.truncated,
            cursor: page.cursor.map(DataKeyDto::from),
        }
    }
}

impl From<tooling::DataChildrenPage> for DataChildViewsResult {
    fn from(page: tooling::DataChildrenPage) -> Self {
        Self {
            children: page
                .children
                .into_iter()
                .map(DataChildViewDto::from)
                .collect(),
            truncated: page.truncated,
            cursor: page.cursor.map(DataKeyDto::from),
        }
    }
}

impl From<DataPresence> for DataPresenceDto {
    fn from(presence: DataPresence) -> Self {
        match presence {
            DataPresence::Absent => Self::Absent,
            DataPresence::ValueOnly => Self::ValueOnly,
            DataPresence::ChildrenOnly => Self::ChildrenOnly,
        }
    }
}

impl From<ToolingError> for DataChildrenError {
    fn from(error: ToolingError) -> Self {
        match error {
            ToolingError::Query(error) => Self::Path(DataPathErrorDto::from(error)),
            ToolingError::Store(error) => Self::Store(DataStoreErrorDto::from(error)),
        }
    }
}

impl From<StoreError> for DataStoreErrorDto {
    fn from(error: StoreError) -> Self {
        let code = match &error {
            StoreError::Io { .. } => DataStoreErrorCodeDto::Io,
            StoreError::Locked { .. } => DataStoreErrorCodeDto::Locked,
            StoreError::FormatVersion { .. } => DataStoreErrorCodeDto::FormatVersion,
            StoreError::Corruption { .. } => DataStoreErrorCodeDto::Corruption,
            StoreError::RecoveryRequired => DataStoreErrorCodeDto::RecoveryRequired,
            StoreError::LimitExceeded { .. } => DataStoreErrorCodeDto::LimitExceeded,
            StoreError::InvalidCursor { .. } => DataStoreErrorCodeDto::InvalidCursor,
            StoreError::InvalidTransaction { .. } => DataStoreErrorCodeDto::InvalidTransaction,
            StoreError::ReadOnly { .. } => DataStoreErrorCodeDto::ReadOnly,
        };
        Self {
            code,
            message: error.to_string(),
        }
    }
}

impl From<QueryError> for DataPathErrorDto {
    fn from(error: QueryError) -> Self {
        match error {
            QueryError::MissingRoot => Self::MissingRoot,
            QueryError::UnknownRoot { root } => Self::UnknownRoot { root },
            QueryError::TooManyIdentityKeys { root } => Self::TooManyIdentityKeys { root },
            QueryError::IdentityKeyType {
                root,
                expected,
                found,
            } => Self::IdentityKeyType {
                root,
                expected: ScalarTypeDto::from(expected),
                found: ScalarTypeDto::from(found),
            },
            QueryError::MissingIdentityKeys { root, expected } => {
                Self::MissingIdentityKeys { root, expected }
            }
            QueryError::UnexpectedKey => Self::UnexpectedKey,
            QueryError::UnknownMember { flavor, name } => Self::UnknownMember {
                flavor: MemberFlavorDto::from(flavor),
                name,
            },
            QueryError::TooManyMemberKeys { member } => Self::TooManyMemberKeys { member },
            QueryError::MemberKeyType {
                member,
                expected,
                found,
            } => Self::MemberKeyType {
                member,
                expected: ScalarTypeDto::from(expected),
                found: ScalarTypeDto::from(found),
            },
            QueryError::IncompleteMemberKeys { member } => Self::IncompleteMemberKeys { member },
            QueryError::ZeroLimit => Self::ZeroLimit,
            QueryError::CursorOutsidePath => Self::CursorOutsidePath,
            QueryError::CursorNotAPosition => Self::CursorNotAPosition,
            QueryError::CursorNotAnEntry => Self::CursorNotAnEntry,
            QueryError::MembersTakeNoCursor => Self::MembersTakeNoCursor,
            QueryError::NoChildScan => Self::NoChildScan,
        }
    }
}

impl From<marrow_check::ScalarType> for ScalarTypeDto {
    fn from(ty: marrow_check::ScalarType) -> Self {
        match ty {
            marrow_check::ScalarType::Bool => Self::Bool,
            marrow_check::ScalarType::Int => Self::Int,
            marrow_check::ScalarType::Str => Self::String,
            marrow_check::ScalarType::Bytes => Self::Bytes,
            marrow_check::ScalarType::Date => Self::Date,
            marrow_check::ScalarType::Duration => Self::Duration,
            marrow_check::ScalarType::Instant => Self::Instant,
            marrow_check::ScalarType::Decimal => Self::Decimal,
        }
    }
}

impl From<MemberFlavor> for MemberFlavorDto {
    fn from(flavor: MemberFlavor) -> Self {
        match flavor {
            MemberFlavor::Field => Self::Field,
            MemberFlavor::Layer => Self::Layer,
            MemberFlavor::Member => Self::Member,
        }
    }
}

mod i128_string {
    pub fn serialize<S>(value: &i128, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<i128, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
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
    fn a_missing_reader_answers_unavailable() {
        let program = CheckedProgram::default();
        assert_eq!(
            saved_roots(None, &program),
            SavedRootsResult {
                available: false,
                roots: Vec::new()
            }
        );
    }

    #[test]
    fn data_key_string_uses_string_wire_kind() {
        let key = DataKeyDto::String("alpha".into());
        assert_eq!(
            serde_json::to_value(&key).unwrap(),
            serde_json::json!({ "kind": "string", "value": "alpha" })
        );
        assert_eq!(SavedKey::from(key), SavedKey::Str("alpha".into()));
        assert_eq!(
            DataKeyDto::from(SavedKey::Str("alpha".into())),
            DataKeyDto::String("alpha".into())
        );
    }

    #[test]
    fn data_key_temporal_values_use_string_wire_payloads() {
        let key = DataKeyDto::Instant(i128::MAX);
        let json = serde_json::json!({
            "kind": "instant",
            "value": i128::MAX.to_string(),
        });
        assert_eq!(serde_json::to_value(&key).unwrap(), json);
        assert_eq!(serde_json::from_value::<DataKeyDto>(json).unwrap(), key);

        let result = DataChildrenResult {
            children: vec![DataChildDto::Key(DataKeyDto::Duration(i128::MIN))],
            truncated: true,
            cursor: Some(DataKeyDto::Instant(i128::MAX)),
        };
        let json = serde_json::json!({
            "children": [
                {
                    "kind": "key",
                    "value": {
                        "kind": "duration",
                        "value": i128::MIN.to_string(),
                    },
                },
            ],
            "truncated": true,
            "cursor": {
                "kind": "instant",
                "value": i128::MAX.to_string(),
            },
        });
        assert_eq!(serde_json::to_value(&result).unwrap(), json);
        assert_eq!(
            serde_json::from_value::<DataChildrenResult>(json).unwrap(),
            result
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
            },
        )
        .unwrap();

        assert_eq!(
            read,
            DataReadResult {
                presence: DataPresenceDto::ValueOnly,
                value: Some("42".into()),
            }
        );
    }

    #[test]
    fn data_read_truncates_large_value_preview() {
        let long = "a".repeat(DATA_VALUE_PREVIEW_LIMIT + 20);
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
            },
        )
        .unwrap();

        let value = read.value.expect("value preview");
        assert_eq!(read.presence, DataPresenceDto::ValueOnly);
        assert!(value.ends_with("..."), "{value}");
        assert!(value.len() <= DATA_VALUE_PREVIEW_LIMIT + 3, "{value}");
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
