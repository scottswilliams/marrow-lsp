//! The Data Explorer custom-request core: read-only saved-tree inspection the
//! editor's tree view drives over three custom LSP methods.
//!
//! The JSON path and key encodings mirror `marrow serve`'s protocol so a client
//! speaks one saved-path dialect across both surfaces. A path is an array of
//! one-field segment objects (`{root}/{key}/{field}/{layer}/{index}/{index_key}`),
//! and a key is a one-field type-tagged object (`{int}`/`{str}`/`{bool}`/…). Every
//! handler reads through a short-lived [`StoreReader`]; a store that cannot be
//! read right now answers with an `available: false` envelope rather than an
//! error, so the tree view shows an "unavailable" state instead of failing.

use serde::{Deserialize, Serialize};

use marrow_check::CheckedProgram;
use marrow_store::backend::Presence;
use marrow_store::path::{ChildSegment, PathSegment, SavedKey};

use crate::store::{Availability, Children, StoreReader, StoredValue};

/// A key value in a `key`/`index_key` segment and in `savedChildren` output. The
/// tag names the key type, mirroring the serve protocol's key encoding. Wide
/// integers (`duration`, `instant`) and `bytes` carry strings because JSON
/// numbers cannot hold them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyJson {
    Int(i64),
    Bool(bool),
    Str(String),
    /// Days since the Unix epoch.
    Date(i32),
    /// Nanoseconds, as a decimal string (128-bit).
    Duration(String),
    /// Nanoseconds since the epoch, as a decimal string (128-bit).
    Instant(String),
    /// Standard padded base64.
    Bytes(String),
}

impl KeyJson {
    fn into_key(self) -> Option<SavedKey> {
        Some(match self {
            KeyJson::Int(value) => SavedKey::Int(value),
            KeyJson::Bool(value) => SavedKey::Bool(value),
            KeyJson::Str(value) => SavedKey::Str(value),
            KeyJson::Date(value) => SavedKey::Date(value),
            KeyJson::Duration(text) => SavedKey::Duration(text.parse().ok()?),
            KeyJson::Instant(text) => SavedKey::Instant(text.parse().ok()?),
            KeyJson::Bytes(text) => SavedKey::Bytes(base64_decode(&text)?),
        })
    }

    fn from_key(key: &SavedKey) -> Self {
        match key {
            SavedKey::Int(value) => KeyJson::Int(*value),
            SavedKey::Bool(value) => KeyJson::Bool(*value),
            SavedKey::Str(value) => KeyJson::Str(value.clone()),
            SavedKey::Date(value) => KeyJson::Date(*value),
            SavedKey::Duration(value) => KeyJson::Duration(value.to_string()),
            SavedKey::Instant(value) => KeyJson::Instant(value.to_string()),
            SavedKey::Bytes(value) => KeyJson::Bytes(base64_encode(value)),
        }
    }
}

/// One path segment in a request `path`. Tagged by kind, mirroring the serve
/// protocol's path encoding. `field`, `layer`, and `index` all decode to a named
/// segment; the schema, not the bytes, tells them apart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentJson {
    Root(String),
    Key(KeyJson),
    Field(String),
    Layer(String),
    Index(String),
    IndexKey(KeyJson),
}

impl SegmentJson {
    fn into_segment(self) -> Option<PathSegment> {
        Some(match self {
            SegmentJson::Root(name) => PathSegment::Root(name),
            SegmentJson::Key(key) => PathSegment::RecordKey(key.into_key()?),
            SegmentJson::Field(name) => PathSegment::Field(name),
            SegmentJson::Layer(name) => PathSegment::ChildLayer(name),
            SegmentJson::Index(name) => PathSegment::Index(name),
            SegmentJson::IndexKey(key) => PathSegment::IndexKey(key.into_key()?),
        })
    }
}

/// Decode a JSON path (segments, root-first) to store path segments, or `None`
/// when any segment carries a malformed key. An empty array decodes to the empty
/// path (the level above the roots).
fn decode_path(path: Vec<SegmentJson>) -> Option<Vec<PathSegment>> {
    path.into_iter().map(SegmentJson::into_segment).collect()
}

/// `marrow/savedRoots` reply: the saved root names, plus whether the store could
/// be read. When `available` is false the roots are empty and the tree view shows
/// an unavailable state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRootsResult {
    pub available: bool,
    pub roots: Vec<String>,
}

/// `marrow/savedChildren` request: the parent path whose children to list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedChildrenParams {
    pub path: Vec<SegmentJson>,
}

/// One child of a path: a record/index key or a named member. Mirrors the serve
/// protocol's `saved_children` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChildJson {
    Key { key: KeyJson },
    Name { name: String },
}

/// `marrow/savedChildren` reply: the immediate children, whether more remained
/// past the cap, and store availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedChildrenResult {
    pub available: bool,
    pub children: Vec<ChildJson>,
    /// True when the store held more children than the page cap.
    pub more: bool,
}

/// `marrow/savedGet` request: the exact path to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedGetParams {
    pub path: Vec<SegmentJson>,
}

/// `marrow/savedGet` reply: presence, the rendered value when one is stored, and
/// the schema-resolved type name of the leaf. Mirrors the serve protocol's
/// presence words, but `value` is the schema-typed rendered string (not raw
/// base64) since the language server has the program to type it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedGetResult {
    pub available: bool,
    /// `absent`, `value_only`, `children_only`, or `value_and_children`.
    pub presence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// The scalar type name of the leaf (`int`, `string`, …), when the path is a
    /// declared scalar; absent for a record node, an index marker, or an orphan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
}

/// Handle `marrow/savedRoots`. A `None` reader (no native store configured) or an
/// unavailable store both answer `available: false` with no roots.
pub fn saved_roots(reader: Option<&StoreReader>) -> SavedRootsResult {
    match reader.map(StoreReader::roots) {
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

/// Handle `marrow/savedChildren`. A malformed key in the path, a missing reader,
/// or an unavailable store all answer `available: false`.
pub fn saved_children(
    params: SavedChildrenParams,
    reader: Option<&StoreReader>,
) -> SavedChildrenResult {
    let unavailable = SavedChildrenResult {
        available: false,
        children: Vec::new(),
        more: false,
    };
    let Some(path) = decode_path(params.path) else {
        return unavailable;
    };
    match reader.map(|reader| reader.children(&path)) {
        Some(Availability::Available(Children { entries, more })) => SavedChildrenResult {
            available: true,
            children: entries.iter().map(child_json).collect(),
            more,
        },
        _ => unavailable,
    }
}

/// Handle `marrow/savedGet`. The path is typed against `program` so a scalar leaf
/// reports its type and renders its value. A malformed key, a missing reader, or
/// an unavailable store answer `available: false`.
pub fn saved_get(
    params: SavedGetParams,
    program: &CheckedProgram,
    reader: Option<&StoreReader>,
) -> SavedGetResult {
    let unavailable = SavedGetResult {
        available: false,
        presence: presence_word(Presence::Absent).to_string(),
        value: None,
        r#type: None,
    };
    let Some(path) = decode_path(params.path) else {
        return unavailable;
    };
    let type_name = scalar_type_name(program, &path);
    match reader.map(|reader| reader.get(&path, program)) {
        Some(Availability::Available(StoredValue { presence, value })) => SavedGetResult {
            available: true,
            presence: presence_word(presence).to_string(),
            value,
            r#type: type_name,
        },
        _ => unavailable,
    }
}

/// The leaf scalar type name for a path, when the schema declares it a scalar.
fn scalar_type_name(program: &CheckedProgram, path: &[PathSegment]) -> Option<String> {
    match marrow_run::classify_saved_path(program, path) {
        marrow_run::SavedPathClass::Scalar(ty) => Some(ty.name().to_string()),
        _ => None,
    }
}

fn child_json(child: &ChildSegment) -> ChildJson {
    match child {
        ChildSegment::Key(key) => ChildJson::Key {
            key: KeyJson::from_key(key),
        },
        ChildSegment::Name(name) => ChildJson::Name { name: name.clone() },
    }
}

/// The stable presence word for the wire, matching the serve protocol's table.
fn presence_word(presence: Presence) -> &'static str {
    match presence {
        Presence::Absent => "absent",
        Presence::ValueOnly => "value_only",
        Presence::ChildrenOnly => "children_only",
        Presence::ValueAndChildren => "value_and_children",
    }
}

/// Standard padded base64 (RFC 4648), the one dialect the serve surface uses.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;
        out.push(ALPHABET[b0 >> 2] as char);
        out.push(ALPHABET[((b0 & 0b11) << 4) | (b1 >> 4)] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((b1 & 0b1111) << 2) | (b2 >> 6)] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[b2 & 0b111111] as char
        } else {
            '='
        });
    }
    out
}

/// Decode standard padded base64, or `None` for invalid or unpadded text.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    // Empty text is the encoding of empty bytes; any other length must be a whole
    // number of 4-char groups.
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    if bytes.len() % 4 != 0 {
        return None;
    }
    let value = |byte: u8| -> Option<u8> {
        Some(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&b| b == b'=').count();
        if pad > 2 {
            return None;
        }
        let sextet = |i: usize| -> Option<u8> {
            if i < 4 - pad {
                value(chunk[i])
            } else if chunk[i] == b'=' {
                Some(0)
            } else {
                None
            }
        };
        let s0 = sextet(0)?;
        let s1 = sextet(1)?;
        let s2 = sextet(2)?;
        let s3 = sextet(3)?;
        out.push((s0 << 2) | (s1 >> 4));
        if pad < 2 {
            out.push((s1 << 4) | (s2 >> 2));
        }
        if pad < 1 {
            out.push((s2 << 6) | s3);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_round_trips_through_its_json_encoding() {
        let json = vec![
            SegmentJson::Root("books".to_string()),
            SegmentJson::Key(KeyJson::Int(1)),
            SegmentJson::Field("title".to_string()),
        ];
        let segments = decode_path(json).expect("decodes");
        assert_eq!(
            segments,
            vec![
                PathSegment::Root("books".to_string()),
                PathSegment::RecordKey(SavedKey::Int(1)),
                PathSegment::Field("title".to_string()),
            ]
        );
    }

    #[test]
    fn a_segment_deserializes_from_the_documented_json() {
        let value = serde_json::json!({ "key": { "int": 7 } });
        let segment: SegmentJson = serde_json::from_value(value).unwrap();
        assert_eq!(segment, SegmentJson::Key(KeyJson::Int(7)));

        let value = serde_json::json!({ "root": "books" });
        let segment: SegmentJson = serde_json::from_value(value).unwrap();
        assert_eq!(segment, SegmentJson::Root("books".to_string()));
    }

    #[test]
    fn a_child_key_serializes_with_its_kind_tag() {
        let child = child_json(&ChildSegment::Key(SavedKey::Int(2)));
        let value = serde_json::to_value(&child).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "kind": "key", "key": { "int": 2 } })
        );

        let child = child_json(&ChildSegment::Name("title".to_string()));
        let value = serde_json::to_value(&child).unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "kind": "name", "name": "title" })
        );
    }

    #[test]
    fn base64_round_trips_and_rejects_unpadded() {
        for sample in [b"Mort".as_slice(), b"", b"\x00\x01\x02\xff", b"a"] {
            let encoded = base64_encode(sample);
            assert_eq!(base64_decode(&encoded).as_deref(), Some(sample));
        }
        // `Mort` is `TW9ydA==`; the unpadded form is rejected.
        assert_eq!(base64_encode(b"Mort"), "TW9ydA==");
        assert!(base64_decode("TW9ydA").is_none());
    }

    #[test]
    fn a_missing_reader_answers_unavailable() {
        assert_eq!(
            saved_roots(None),
            SavedRootsResult {
                available: false,
                roots: Vec::new()
            }
        );
        let children = saved_children(
            SavedChildrenParams {
                path: vec![SegmentJson::Root("books".to_string())],
            },
            None,
        );
        assert!(!children.available);
    }

    #[test]
    fn saved_get_reports_the_leaf_type() {
        // The handlers type the path even when the store is unavailable, but the
        // type is only attached on a successful read; with no reader the result is
        // unavailable and carries no type. The type-name helper is covered here in
        // isolation against a real schema.
        use marrow_check::{ProjectSources, analyze_project};
        use marrow_project::parse_config;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("a.mw"),
            "module a\n\nresource Book at ^books(id: int)\n    required title: string\n",
        )
        .unwrap();
        let config = parse_config(r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let program = analyze_project(root, &config, &ProjectSources::new())
            .unwrap()
            .program;
        let path = vec![
            PathSegment::Root("books".to_string()),
            PathSegment::RecordKey(SavedKey::Int(1)),
            PathSegment::Field("title".to_string()),
        ];
        assert_eq!(scalar_type_name(&program, &path).as_deref(), Some("string"));
    }
}
