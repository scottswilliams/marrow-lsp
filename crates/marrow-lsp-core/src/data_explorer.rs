//! Saved-data presentation DTOs for editor and tool transports.
//!
//! Marrow owns saved-data traversal and typed key validation. This module only
//! translates transport DTOs, clamps child pages, and calls Marrow tooling APIs.

use serde::{Deserialize, Serialize};

use marrow_check::CheckedProgram;
use marrow_check::tooling::{
    self, DataChild, DataQuerySegment, MemberFlavor, QueryError, ToolingError,
};
use marrow_store::StoreError;
use marrow_store::key::SavedKey;
use marrow_store::tree::TreeStore;

use crate::store::{Availability, LiveStore};

pub const DATA_CHILDREN_PAGE_LIMIT: usize = 200;

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
pub enum DataQuerySegmentDto {
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
    Key(DataKeyDto),
    Member(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataChildrenRequest {
    pub segments: Vec<DataQuerySegmentDto>,
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
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DataChildrenError {
    Query(DataQueryErrorDto),
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
pub enum DataQueryErrorDto {
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
    let page = tooling::data_children(
        program,
        store,
        &segments,
        limit.min(DATA_CHILDREN_PAGE_LIMIT),
        cursor.as_ref(),
    )?;
    Ok(DataChildrenResult::from(page))
}

impl From<DataQuerySegmentDto> for DataQuerySegment {
    fn from(segment: DataQuerySegmentDto) -> Self {
        match segment {
            DataQuerySegmentDto::Root(root) => Self::Root(root),
            DataQuerySegmentDto::Field(field) => Self::Field(field),
            DataQuerySegmentDto::Layer(layer) => Self::Layer(layer),
            DataQuerySegmentDto::Key(key) => Self::Key(SavedKey::from(key)),
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
            DataChild::Key(key) => Self::Key(DataKeyDto::from(key)),
            DataChild::Member(member) => Self::Member(member),
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

impl From<ToolingError> for DataChildrenError {
    fn from(error: ToolingError) -> Self {
        match error {
            ToolingError::Query(error) => Self::Query(DataQueryErrorDto::from(error)),
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

impl From<QueryError> for DataQueryErrorDto {
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

    use marrow_check::test_support::{commit_then_check, root_place, store_id_of};
    use marrow_store::key::SavedKey;
    use marrow_store::tree::TreeStore;

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
    fn zero_limit_returns_query_error() {
        let (program, store) = checked_counter_store(1..=1);
        assert_eq!(
            data_children_page(
                &program,
                &store,
                DataChildrenRequest {
                    segments: vec![DataQuerySegmentDto::Root("counter".into())],
                    limit: 0,
                    cursor: None,
                },
            ),
            Err(DataChildrenError::Query(DataQueryErrorDto::ZeroLimit))
        );
    }

    #[test]
    fn typed_query_error_reports_string_scalar_type() {
        let (program, store) =
            checked_counter_store_with_keys("string", std::iter::empty::<SavedKey>());
        assert_eq!(
            data_children_page(
                &program,
                &store,
                DataChildrenRequest {
                    segments: vec![
                        DataQuerySegmentDto::Root("counter".into()),
                        DataQuerySegmentDto::Key(DataKeyDto::Int(1)),
                    ],
                    limit: 2,
                    cursor: None,
                },
            ),
            Err(DataChildrenError::Query(
                DataQueryErrorDto::IdentityKeyType {
                    root: "counter".into(),
                    expected: ScalarTypeDto::String,
                    found: ScalarTypeDto::Int,
                }
            ))
        );
    }

    #[test]
    fn data_children_clamps_large_caller_limit() {
        let (program, store) = checked_counter_store(1..=201);
        let page = data_children_page(
            &program,
            &store,
            DataChildrenRequest {
                segments: vec![DataQuerySegmentDto::Root("counter".into())],
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
        let root = vec![DataQuerySegmentDto::Root("counter".into())];

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

    fn checked_counter_store(ids: std::ops::RangeInclusive<i64>) -> (CheckedProgram, TreeStore) {
        checked_counter_store_with_keys("int", ids.map(SavedKey::Int))
    }

    fn checked_counter_store_with_keys(
        key_type: &str,
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
    required value: int

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
