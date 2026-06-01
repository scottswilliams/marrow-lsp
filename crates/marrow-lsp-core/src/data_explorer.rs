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
use marrow_schema::{Element, IndexSchema, KeyDef, Node, ResourceSchema, Type};
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
            SegmentJson::Layer(name) | SegmentJson::Index(name) => PathSegment::Field(name),
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
    Key {
        key: KeyJson,
        #[serde(skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        r#type: Option<String>,
        #[serde(rename = "appendSegment", skip_serializing_if = "Option::is_none")]
        append_segment: Option<SegmentJson>,
    },
    Name {
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        r#type: Option<String>,
        #[serde(rename = "appendSegment", skip_serializing_if = "Option::is_none")]
        append_segment: Option<SegmentJson>,
    },
}

/// Optional schema metadata for a child. These fields are absent from the raw
/// compatibility path and present only when the LSP Data Explorer asks with a
/// checked program.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub append_segment: Option<SegmentJson>,
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
    /// The declared type name of the leaf (`int`, `string`, `Resource::Id`, …),
    /// when the path is a typed leaf; absent for a record node, an index marker,
    /// or an orphan.
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

/// Handle `marrow/savedChildren` for the editor Data Explorer. This preserves the
/// raw child shape and adds optional schema metadata for the immediate children
/// of the requested path.
pub fn saved_children_with_schema(
    params: SavedChildrenParams,
    program: &CheckedProgram,
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
        Some(Availability::Available(Children { entries, more })) => {
            let context = schema_context(program, &path);
            SavedChildrenResult {
                available: true,
                children: entries
                    .iter()
                    .map(|child| child_json_with_metadata(child, child_metadata(&context, child)))
                    .collect(),
                more,
            }
        }
        _ => unavailable,
    }
}

/// Handle `marrow/savedGet`. The path is typed against `program` so a typed leaf
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
    let type_name = leaf_type_name(program, &path);
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

enum SchemaContext<'a> {
    Unknown,
    Root {
        resource: &'a ResourceSchema,
        next_key: usize,
        include_indexes: bool,
    },
    Members {
        members: &'a [Node],
    },
    LayerKeys {
        node: &'a Node,
        next_key: usize,
    },
    IndexKeys {
        resource: &'a ResourceSchema,
        index: &'a IndexSchema,
        next_key: usize,
    },
    Leaf,
}

fn schema_context<'a>(program: &'a CheckedProgram, path: &[PathSegment]) -> SchemaContext<'a> {
    let Some((PathSegment::Root(root), rest)) = path.split_first() else {
        return SchemaContext::Unknown;
    };
    let Some(resource) = marrow_check::resolve::resolve_resource_by_root(program, root) else {
        return SchemaContext::Unknown;
    };
    let Some(saved_root) = resource.saved_root.as_ref() else {
        return SchemaContext::Unknown;
    };
    let identity_arity = saved_root.identity_keys.len();

    if rest.is_empty() {
        return if identity_arity == 0 {
            SchemaContext::Members {
                members: &resource.members,
            }
        } else {
            SchemaContext::Root {
                resource,
                next_key: 0,
                include_indexes: true,
            }
        };
    }

    if let Some((name, index_keys)) = named_branch(rest)
        && let Some(index) = resource.indexes.iter().find(|index| index.name == name)
    {
        return if index_keys < rest.len() - 1 || index_keys >= index.args.len() {
            SchemaContext::Leaf
        } else {
            SchemaContext::IndexKeys {
                resource,
                index,
                next_key: index_keys,
            }
        };
    }

    let identity_keys = rest
        .iter()
        .take_while(|segment| matches!(segment, PathSegment::RecordKey(_)))
        .count();
    if identity_keys < identity_arity {
        return if identity_keys == rest.len() {
            SchemaContext::Root {
                resource,
                next_key: identity_keys,
                include_indexes: false,
            }
        } else {
            SchemaContext::Unknown
        };
    }
    if identity_keys != identity_arity {
        return SchemaContext::Unknown;
    }

    let members = &rest[identity_keys..];
    if members.is_empty() {
        return SchemaContext::Members {
            members: &resource.members,
        };
    }
    member_context(&resource.members, members)
}

fn named_branch(segments: &[PathSegment]) -> Option<(&str, usize)> {
    let (first, rest) = segments.split_first()?;
    let name = segment_name(first)?;
    let key_count = rest
        .iter()
        .take_while(|segment| matches!(segment, PathSegment::IndexKey(_)))
        .count();
    Some((name, key_count))
}

fn member_context<'a>(mut members: &'a [Node], mut rest: &[PathSegment]) -> SchemaContext<'a> {
    loop {
        let Some((segment, tail)) = rest.split_first() else {
            return SchemaContext::Members { members };
        };
        let Some(name) = segment_name(segment) else {
            return SchemaContext::Unknown;
        };
        let Some(node) = members.iter().find(|node| node.name == name) else {
            return SchemaContext::Unknown;
        };
        let key_count = tail
            .iter()
            .take_while(|segment| matches!(segment, PathSegment::IndexKey(_)))
            .count();
        let after_keys = &tail[key_count..];
        if key_count < node.key_params.len() {
            return if after_keys.is_empty() {
                SchemaContext::LayerKeys {
                    node,
                    next_key: key_count,
                }
            } else {
                SchemaContext::Unknown
            };
        }
        if key_count > node.key_params.len() {
            return SchemaContext::Unknown;
        }
        if after_keys.is_empty() {
            return match &node.element {
                Element::Group => SchemaContext::Members {
                    members: &node.members,
                },
                Element::Slot { .. } => SchemaContext::Leaf,
            };
        }
        if !matches!(node.element, Element::Group) {
            return SchemaContext::Unknown;
        }
        members = &node.members;
        rest = after_keys;
    }
}

fn segment_name(segment: &PathSegment) -> Option<&str> {
    match segment {
        PathSegment::Field(name) | PathSegment::ChildLayer(name) | PathSegment::Index(name) => {
            Some(name)
        }
        PathSegment::Root(_) | PathSegment::RecordKey(_) | PathSegment::IndexKey(_) => None,
    }
}

fn child_metadata(context: &SchemaContext<'_>, child: &ChildSegment) -> ChildMetadata {
    match (context, child) {
        (
            SchemaContext::Root {
                resource, next_key, ..
            },
            ChildSegment::Key(key),
        ) => resource
            .saved_root
            .as_ref()
            .and_then(|root| root.identity_keys.get(*next_key))
            .map(|key_def| {
                key_metadata(
                    "record_key",
                    key_def,
                    SegmentJson::Key(KeyJson::from_key(key)),
                )
            })
            .unwrap_or_default(),
        (
            SchemaContext::Root {
                resource,
                include_indexes: true,
                ..
            },
            ChildSegment::Name(name),
        ) => resource
            .indexes
            .iter()
            .find(|index| index.name == *name)
            .map(index_metadata)
            .unwrap_or_default(),
        (SchemaContext::Members { members }, ChildSegment::Name(name)) => members
            .iter()
            .find(|node| node.name == *name)
            .map(node_metadata)
            .unwrap_or_default(),
        (SchemaContext::LayerKeys { node, next_key }, ChildSegment::Key(key)) => node
            .key_params
            .get(*next_key)
            .map(|key_def| {
                key_metadata(
                    "layer_key",
                    key_def,
                    SegmentJson::IndexKey(KeyJson::from_key(key)),
                )
            })
            .unwrap_or_default(),
        (
            SchemaContext::IndexKeys {
                resource,
                index,
                next_key,
            },
            ChildSegment::Key(key),
        ) => index
            .args
            .get(*next_key)
            .and_then(|arg| index_arg_type(resource, arg).map(|ty| (arg, ty)))
            .map(|(arg, ty)| {
                let ty = type_name(ty);
                ChildMetadata {
                    role: Some("index_key".to_string()),
                    detail: Some(format!("{arg}: {ty}")),
                    r#type: Some(ty),
                    append_segment: Some(SegmentJson::IndexKey(KeyJson::from_key(key))),
                }
            })
            .unwrap_or_default(),
        _ => ChildMetadata::default(),
    }
}

fn key_metadata(role: &str, key: &KeyDef, append_segment: SegmentJson) -> ChildMetadata {
    let ty = type_name(&key.ty);
    ChildMetadata {
        role: Some(role.to_string()),
        detail: Some(format!("{}: {ty}", key.name)),
        r#type: Some(ty),
        append_segment: Some(append_segment),
    }
}

fn index_metadata(index: &IndexSchema) -> ChildMetadata {
    ChildMetadata {
        role: Some("index".to_string()),
        detail: Some(format!("index {}({})", index.name, index.args.join(", "))),
        r#type: None,
        append_segment: Some(SegmentJson::Index(index.name.clone())),
    }
}

fn node_metadata(node: &Node) -> ChildMetadata {
    match &node.element {
        Element::Slot { ty, required } if node.key_params.is_empty() => ChildMetadata {
            role: Some("field".to_string()),
            detail: required.then(|| "required".to_string()),
            r#type: Some(type_name(ty)),
            append_segment: Some(SegmentJson::Field(node.name.clone())),
        },
        Element::Slot { ty, .. } => ChildMetadata {
            role: Some("layer".to_string()),
            detail: key_params_detail(&node.key_params),
            r#type: Some(type_name(ty)),
            append_segment: Some(SegmentJson::Layer(node.name.clone())),
        },
        Element::Group => ChildMetadata {
            role: Some(if node.key_params.is_empty() {
                "group".to_string()
            } else {
                "layer".to_string()
            }),
            detail: key_params_detail(&node.key_params),
            r#type: None,
            append_segment: Some(SegmentJson::Layer(node.name.clone())),
        },
    }
}

fn index_arg_type<'a>(resource: &'a ResourceSchema, arg: &str) -> Option<&'a Type> {
    resource
        .saved_root
        .as_ref()
        .and_then(|root| root.identity_keys.iter().find(|key| key.name == arg))
        .map(|key| &key.ty)
        .or_else(|| resource.field_type(&[arg]))
}

fn key_params_detail(params: &[KeyDef]) -> Option<String> {
    if params.is_empty() {
        return None;
    }
    Some(
        params
            .iter()
            .map(|key| format!("{}: {}", key.name, type_name(&key.ty)))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

fn type_name(ty: &Type) -> String {
    ty.to_string()
}

/// The leaf type name for a path, when the schema declares it as a typed leaf.
fn leaf_type_name(program: &CheckedProgram, path: &[PathSegment]) -> Option<String> {
    match marrow_run::classify_saved_path(program, path) {
        marrow_run::SavedPathClass::Scalar(ty) => Some(ty.name().to_string()),
        marrow_run::SavedPathClass::Identity { resource, .. } => Some(format!("{resource}::Id")),
        _ => None,
    }
}

fn child_json(child: &ChildSegment) -> ChildJson {
    child_json_with_metadata(child, ChildMetadata::default())
}

fn child_json_with_metadata(child: &ChildSegment, metadata: ChildMetadata) -> ChildJson {
    let ChildMetadata {
        role,
        detail,
        r#type,
        append_segment,
    } = metadata;
    match child {
        ChildSegment::Key(key) => ChildJson::Key {
            key: KeyJson::from_key(key),
            role,
            detail,
            r#type,
            append_segment,
        },
        ChildSegment::Name(name) => ChildJson::Name {
            name: name.clone(),
            role,
            detail,
            r#type,
            append_segment,
        },
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
    if !bytes.len().is_multiple_of(4) {
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

    struct SchemaFixture {
        _dir: tempfile::TempDir,
        project: crate::workspace::Project,
        program: CheckedProgram,
        store_path: std::path::PathBuf,
    }

    fn schema_fixture(source: &str) -> SchemaFixture {
        use marrow_check::{ProjectSources, analyze_project};
        use marrow_project::parse_config;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let config_text = r#"{
            "sourceRoots": ["src"],
            "store": { "backend": "native", "dataDir": "data" }
        }"#;
        std::fs::write(root.join("marrow.json"), config_text).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("src").join("shelf.mw"), source).unwrap();

        let config = parse_config(config_text).unwrap();
        let program = analyze_project(root, &config, &ProjectSources::new())
            .unwrap()
            .program;
        let project = crate::workspace::Project {
            root: root.to_path_buf(),
            config,
        };
        let store_path = root.join("data").join("marrow.redb");
        SchemaFixture {
            _dir: dir,
            project,
            program,
            store_path,
        }
    }

    fn schema_children_source() -> &'static str {
        "\
module shelf

resource Book at ^books(id: int)
    required title: string
    tags(pos: int): string

    index byTitle(title) unique

resource Edition at ^editions(isbn: string, locale: string)
    required title: string
"
    }

    fn seed_schema_children_store(path: &std::path::Path) {
        use marrow_store::backend::Backend;
        use marrow_store::path::{PathSegment, SavedKey, encode_key_value, encode_path};
        use marrow_store::redb::RedbStore;

        let mut store = RedbStore::open(path).unwrap();
        store
            .write(
                &encode_path(&[
                    PathSegment::Root("books".to_string()),
                    PathSegment::RecordKey(SavedKey::Int(1)),
                    PathSegment::Field("title".to_string()),
                    PathSegment::IndexKey(SavedKey::Int(0)),
                    PathSegment::Field("note".to_string()),
                ]),
                b"leaf orphan".to_vec(),
            )
            .unwrap();
        store
            .write(
                &encode_path(&[
                    PathSegment::Root("books".to_string()),
                    PathSegment::RecordKey(SavedKey::Int(1)),
                    PathSegment::Field("title".to_string()),
                ]),
                b"Mort".to_vec(),
            )
            .unwrap();
        store
            .write(
                &encode_path(&[
                    PathSegment::Root("books".to_string()),
                    PathSegment::RecordKey(SavedKey::Int(1)),
                    PathSegment::ChildLayer("tags".to_string()),
                    PathSegment::IndexKey(SavedKey::Int(0)),
                ]),
                b"fantasy".to_vec(),
            )
            .unwrap();
        store
            .write(
                &encode_path(&[
                    PathSegment::Root("books".to_string()),
                    PathSegment::Index("byTitle".to_string()),
                    PathSegment::IndexKey(SavedKey::Str("Mort".to_string())),
                    PathSegment::IndexKey(SavedKey::Int(0)),
                    PathSegment::Field("note".to_string()),
                ]),
                b"index orphan".to_vec(),
            )
            .unwrap();
        store
            .write(
                &encode_path(&[
                    PathSegment::Root("books".to_string()),
                    PathSegment::Index("byTitle".to_string()),
                    PathSegment::IndexKey(SavedKey::Str("Mort".to_string())),
                ]),
                encode_key_value(&SavedKey::Int(1)),
            )
            .unwrap();
        store
            .write(
                &encode_path(&[
                    PathSegment::Root("books".to_string()),
                    PathSegment::RecordKey(SavedKey::Int(1)),
                    PathSegment::Field("legacy".to_string()),
                ]),
                b"foreign".to_vec(),
            )
            .unwrap();
        store
            .write(
                &encode_path(&[
                    PathSegment::Root("books".to_string()),
                    PathSegment::RecordKey(SavedKey::Int(1)),
                    PathSegment::Field("legacy".to_string()),
                    PathSegment::IndexKey(SavedKey::Int(0)),
                    PathSegment::Field("note".to_string()),
                ]),
                b"orphan".to_vec(),
            )
            .unwrap();
        store
            .write(
                &encode_path(&[
                    PathSegment::Root("editions".to_string()),
                    PathSegment::RecordKey(SavedKey::Str("978-0".to_string())),
                    PathSegment::RecordKey(SavedKey::Str("en".to_string())),
                    PathSegment::Field("title".to_string()),
                ]),
                b"Mort".to_vec(),
            )
            .unwrap();
        store
            .write(
                &encode_path(&[
                    PathSegment::Root("orphans".to_string()),
                    PathSegment::RecordKey(SavedKey::Int(0)),
                    PathSegment::Field("note".to_string()),
                ]),
                b"schema orphan".to_vec(),
            )
            .unwrap();
    }

    fn assert_child_json(children: &[ChildJson], expected: serde_json::Value) {
        let actual: Vec<serde_json::Value> = children
            .iter()
            .map(|child| serde_json::to_value(child).unwrap())
            .collect();
        assert!(
            actual.iter().any(|child| child == &expected),
            "missing child {expected}; actual children: {actual:#?}"
        );
    }

    fn assert_no_child_metadata(children: &[ChildJson], kind: &str, name: &str) {
        let actual = children
            .iter()
            .map(|child| serde_json::to_value(child).unwrap())
            .find(|child| child["kind"] == kind && child["name"] == name)
            .unwrap_or_else(|| panic!("missing {kind} child {name}"));
        assert_eq!(
            actual,
            serde_json::json!({ "kind": kind, "name": name }),
            "orphan/foreign children stay plain when schema metadata is unavailable"
        );
    }

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
    fn saved_children_with_schema_labels_record_members_layers_and_indexes() {
        let fixture = schema_fixture(schema_children_source());
        seed_schema_children_store(&fixture.store_path);
        let reader = StoreReader::for_project(&fixture.project).unwrap();

        let root = saved_children_with_schema(
            SavedChildrenParams {
                path: vec![SegmentJson::Root("books".to_string())],
            },
            &fixture.program,
            Some(&reader),
        );

        assert!(root.available, "{root:?}");
        assert_child_json(
            &root.children,
            serde_json::json!({
                "kind": "key",
                "key": { "int": 1 },
                "role": "record_key",
                "detail": "id: int",
                "type": "int",
                "appendSegment": { "key": { "int": 1 } }
            }),
        );
        assert_child_json(
            &root.children,
            serde_json::json!({
                "kind": "name",
                "name": "byTitle",
                "role": "index",
                "detail": "index byTitle(title)",
                "appendSegment": { "index": "byTitle" }
            }),
        );

        let record = saved_children_with_schema(
            SavedChildrenParams {
                path: vec![
                    SegmentJson::Root("books".to_string()),
                    SegmentJson::Key(KeyJson::Int(1)),
                ],
            },
            &fixture.program,
            Some(&reader),
        );

        assert!(record.available, "{record:?}");
        assert_child_json(
            &record.children,
            serde_json::json!({
                "kind": "name",
                "name": "title",
                "role": "field",
                "detail": "required",
                "type": "string",
                "appendSegment": { "field": "title" }
            }),
        );
        assert_child_json(
            &record.children,
            serde_json::json!({
                "kind": "name",
                "name": "tags",
                "role": "layer",
                "detail": "pos: int",
                "type": "string",
                "appendSegment": { "layer": "tags" }
            }),
        );
        assert_no_child_metadata(&record.children, "name", "legacy");
    }

    #[test]
    fn saved_children_with_schema_uses_path_context_for_key_segments() {
        let fixture = schema_fixture(schema_children_source());
        seed_schema_children_store(&fixture.store_path);
        let reader = StoreReader::for_project(&fixture.project).unwrap();

        let layer = saved_children_with_schema(
            SavedChildrenParams {
                path: vec![
                    SegmentJson::Root("books".to_string()),
                    SegmentJson::Key(KeyJson::Int(1)),
                    SegmentJson::Layer("tags".to_string()),
                ],
            },
            &fixture.program,
            Some(&reader),
        );
        assert!(layer.available, "{layer:?}");
        assert_child_json(
            &layer.children,
            serde_json::json!({
                "kind": "key",
                "key": { "int": 0 },
                "role": "layer_key",
                "detail": "pos: int",
                "type": "int",
                "appendSegment": { "index_key": { "int": 0 } }
            }),
        );

        let index = saved_children_with_schema(
            SavedChildrenParams {
                path: vec![
                    SegmentJson::Root("books".to_string()),
                    SegmentJson::Index("byTitle".to_string()),
                ],
            },
            &fixture.program,
            Some(&reader),
        );
        assert!(index.available, "{index:?}");
        assert_child_json(
            &index.children,
            serde_json::json!({
                "kind": "key",
                "key": { "str": "Mort" },
                "role": "index_key",
                "detail": "title: string",
                "type": "string",
                "appendSegment": { "index_key": { "str": "Mort" } }
            }),
        );

        let composite_record = saved_children_with_schema(
            SavedChildrenParams {
                path: vec![
                    SegmentJson::Root("editions".to_string()),
                    SegmentJson::Key(KeyJson::Str("978-0".to_string())),
                ],
            },
            &fixture.program,
            Some(&reader),
        );
        assert!(composite_record.available, "{composite_record:?}");
        assert_child_json(
            &composite_record.children,
            serde_json::json!({
                "kind": "key",
                "key": { "str": "en" },
                "role": "record_key",
                "detail": "locale: string",
                "type": "string",
                "appendSegment": { "key": { "str": "en" } }
            }),
        );
    }

    #[test]
    fn keyed_children_without_schema_context_do_not_get_append_segments() {
        let fixture = schema_fixture(schema_children_source());
        seed_schema_children_store(&fixture.store_path);
        let reader = StoreReader::for_project(&fixture.project).unwrap();
        let unknown_path = vec![
            SegmentJson::Root("books".to_string()),
            SegmentJson::Key(KeyJson::Int(1)),
            SegmentJson::Field("legacy".to_string()),
        ];

        let schema = saved_children_with_schema(
            SavedChildrenParams {
                path: unknown_path.clone(),
            },
            &fixture.program,
            Some(&reader),
        );
        assert!(schema.available, "{schema:?}");
        assert_child_json(
            &schema.children,
            serde_json::json!({
                "kind": "key",
                "key": { "int": 0 }
            }),
        );

        let leaf_path = vec![
            SegmentJson::Root("books".to_string()),
            SegmentJson::Key(KeyJson::Int(1)),
            SegmentJson::Field("title".to_string()),
        ];
        let schema = saved_children_with_schema(
            SavedChildrenParams {
                path: leaf_path.clone(),
            },
            &fixture.program,
            Some(&reader),
        );
        assert!(schema.available, "{schema:?}");
        assert_child_json(
            &schema.children,
            serde_json::json!({
                "kind": "key",
                "key": { "int": 0 }
            }),
        );

        let index_result_path = vec![
            SegmentJson::Root("books".to_string()),
            SegmentJson::Index("byTitle".to_string()),
            SegmentJson::IndexKey(KeyJson::Str("Mort".to_string())),
        ];
        let schema = saved_children_with_schema(
            SavedChildrenParams {
                path: index_result_path.clone(),
            },
            &fixture.program,
            Some(&reader),
        );
        assert!(schema.available, "{schema:?}");
        assert_child_json(
            &schema.children,
            serde_json::json!({
                "kind": "key",
                "key": { "int": 0 }
            }),
        );

        let schema = saved_children_with_schema(
            SavedChildrenParams {
                path: vec![SegmentJson::Root("orphans".to_string())],
            },
            &fixture.program,
            Some(&reader),
        );
        assert!(schema.available, "{schema:?}");
        assert_child_json(
            &schema.children,
            serde_json::json!({
                "kind": "key",
                "key": { "int": 0 }
            }),
        );

        let raw = saved_children(SavedChildrenParams { path: leaf_path }, Some(&reader));
        assert!(raw.available, "{raw:?}");
        assert_child_json(
            &raw.children,
            serde_json::json!({
                "kind": "key",
                "key": { "int": 0 }
            }),
        );
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
        assert_eq!(leaf_type_name(&program, &path).as_deref(), Some("string"));
    }

    #[test]
    fn saved_get_reports_an_identity_leaf_type() {
        use crate::workspace::Project;
        use marrow_check::{ProjectSources, analyze_project};
        use marrow_project::parse_config;
        use marrow_store::backend::Backend;
        use marrow_store::path::{encode_key_value, encode_path};
        use marrow_store::redb::RedbStore;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let config_text = r#"{
            "sourceRoots": ["src"],
            "store": { "backend": "native", "dataDir": "data" }
        }"#;
        std::fs::write(root.join("marrow.json"), config_text).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(
            root.join("src").join("a.mw"),
            "\
module a

resource Author at ^authors(id: int)
    required name: string

resource Book at ^books(id: int)
    authorId: Author::Id
",
        )
        .unwrap();

        let store_path = root.join("data").join("marrow.redb");
        let author_id_path = encode_path(&[
            PathSegment::Root("books".to_string()),
            PathSegment::RecordKey(SavedKey::Int(1)),
            PathSegment::Field("authorId".to_string()),
        ]);
        {
            let mut store = RedbStore::open(&store_path).unwrap();
            store
                .write(&author_id_path, encode_key_value(&SavedKey::Int(7)))
                .unwrap();
        }

        let config = parse_config(config_text).unwrap();
        let program = analyze_project(root, &config, &ProjectSources::new())
            .unwrap()
            .program;
        let project = Project {
            root: root.to_path_buf(),
            config,
        };
        let reader = StoreReader::for_project(&project).unwrap();

        let result = saved_get(
            SavedGetParams {
                path: vec![
                    SegmentJson::Root("books".to_string()),
                    SegmentJson::Key(KeyJson::Int(1)),
                    SegmentJson::Field("authorId".to_string()),
                ],
            },
            &program,
            Some(&reader),
        );

        assert!(result.available, "{result:?}");
        assert_eq!(result.value.as_deref(), Some("Author::Id(7)"));
        assert_eq!(result.r#type.as_deref(), Some("Author::Id"));
    }
}
