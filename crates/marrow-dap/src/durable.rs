//! Raw durable-data inspection for the opt-in DAP debug/admin scope and `^`
//! watches. It renders a synthetic tree of the debuggee's saved roots, expanded
//! lazily through the run's *live* store handle.
//!
//! Unlike the editor's read-only [`StoreReader`](marrow_lsp_core::store::StoreReader)
//! (which opens the on-disk file between calls), this reads the run-thread's own
//! `&RefCell<dyn Backend>` — so it shows read-your-writes: a value written earlier
//! in the run, even inside an uncommitted `transaction`, is visible at the stop.
//! Reading happens only on the run-thread (which holds the borrow); this module is
//! the pure tree-and-render logic it calls.

use marrow_check::CheckedProgram;
use marrow_run::{SavedPathClass, classify_saved_path, decode_identity_arity};
use marrow_store::backend::{Backend, Presence};
use marrow_store::path::{ChildSegment, PathSegment, SavedKey, encode_path};
use marrow_store::value::{Scalar, decode_value, encode_value};

/// One node of the durable tree as the Variables view shows it: the display name
/// under its parent, the rendered value (empty for a pure container), the full
/// path that re-expands it, and whether it has children to expand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableNode {
    pub name: String,
    pub value: String,
    pub path: Vec<PathSegment>,
    pub expandable: bool,
}

/// The saved roots as top-level durable nodes, in Marrow order. Each root is a
/// container the client expands to walk its records.
pub fn roots(store: &dyn Backend) -> Vec<DurableNode> {
    let Ok(names) = store.roots() else {
        return Vec::new();
    };
    names
        .into_iter()
        .map(|name| {
            let path = vec![PathSegment::Root(name.clone())];
            DurableNode {
                name: format!("^{name}"),
                value: String::new(),
                expandable: true,
                path,
            }
        })
        .collect()
}

/// The children of a durable `path`, each rendered as a node. A child that holds
/// a typed stored leaf shows its decoded value; a child that only has descendants
/// shows none and is expandable. The path's schema classification types each leaf
/// so an `int` field reads as a number and a typed reference reads as an identity.
pub fn children(
    store: &dyn Backend,
    program: &CheckedProgram,
    path: &[PathSegment],
) -> Vec<DurableNode> {
    let key = encode_path(path);
    let Ok(child_keys) = store.child_keys(&key) else {
        return Vec::new();
    };
    child_keys
        .into_iter()
        .map(|child| {
            let mut child_path = path.to_vec();
            child_path.push(segment_of(&child));
            node_at(store, program, child_path, child_label(&child))
        })
        .collect()
}

/// A durable node for an exact `path`, reading its stored value (if any) and
/// whether it has children. The label is how it appears under its parent.
fn node_at(
    store: &dyn Backend,
    program: &CheckedProgram,
    path: Vec<PathSegment>,
    label: String,
) -> DurableNode {
    let key = encode_path(&path);
    let value = read_leaf(store, program, &path);
    let expandable = matches!(
        store.presence(&key),
        Ok(Presence::ChildrenOnly) | Ok(Presence::ValueAndChildren)
    );
    DurableNode {
        name: label,
        value: value.unwrap_or_default(),
        path,
        expandable,
    }
}

/// The rendered value stored exactly at `path`, or `None` when nothing is stored
/// there (a pure container node). The path is classified against the schema so a
/// declared scalar decodes to its typed form and a typed-reference leaf renders
/// as `Resource::Id(...)`; corrupt or untyped data renders raw so it stays
/// inspectable rather than vanishing.
pub fn read_leaf(
    store: &dyn Backend,
    program: &CheckedProgram,
    path: &[PathSegment],
) -> Option<String> {
    let key = encode_path(path);
    let bytes = store.read(&key).ok().flatten()?;
    Some(render(&bytes, classify_saved_path(program, path)))
}

/// Render stored bytes using the path's schema class: a declared scalar decodes
/// to its canonical text, a typed reference decodes to an identity constructor,
/// and untyped or corrupt data shows as hex.
fn render(bytes: &[u8], class: SavedPathClass) -> String {
    match class {
        SavedPathClass::Scalar(ty) => match decode_value(bytes, ty) {
            Some(scalar) => render_scalar(&scalar),
            None => hex(bytes),
        },
        SavedPathClass::Identity { resource, arity } => match decode_identity_arity(bytes, arity) {
            Some(keys) => render_identity(&resource, &keys),
            None => hex(bytes),
        },
        SavedPathClass::IndexMarker
        | SavedPathClass::Orphan
        | SavedPathClass::KeyTypeMismatch { .. } => hex(bytes),
    }
}

/// A typed-reference leaf stores another resource's identity key encoding. Show
/// the source-level identity constructor shape in the debugger variables view.
fn render_identity(resource: &str, keys: &[SavedKey]) -> String {
    let args = keys
        .iter()
        .map(marrow_lsp_core::store::saved_key_literal)
        .collect::<Vec<_>>()
        .join(", ");
    format!("{resource}::Id({args})")
}

/// A decoded scalar as display text. Strings are quoted so an empty or
/// whitespace value is unambiguous; date/duration/instant re-encode to their one
/// canonical spelling; every other scalar shows its plain form.
fn render_scalar(scalar: &Scalar) -> String {
    match scalar {
        Scalar::Bool(value) => value.to_string(),
        Scalar::Int(value) => value.to_string(),
        Scalar::Str(text) => format!("\"{text}\""),
        Scalar::Bytes(bytes) => hex(bytes),
        Scalar::Decimal(value) => value.to_text(),
        Scalar::Date(_) | Scalar::Duration(_) | Scalar::Instant(_) => match encode_value(scalar) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(_) => "<unrepresentable>".to_string(),
        },
    }
}

/// Lowercase hex of raw bytes, the fallback for an undecodable or untyped value.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The path segment a child segment extends a parent path by. A keyed child is a
/// record key; a named child is a field, layer, or index name (the schema, not
/// the store, tells them apart, and the durable tree displays all three alike).
fn segment_of(child: &ChildSegment) -> PathSegment {
    match child {
        ChildSegment::Key(key) => PathSegment::RecordKey(key.clone()),
        ChildSegment::Name(name) => PathSegment::Field(name.clone()),
    }
}

/// The label a child shows under its parent: a record key as `(key)`, a named
/// member by its name.
fn child_label(child: &ChildSegment) -> String {
    match child {
        ChildSegment::Key(key) => format!("({})", key_text(key)),
        ChildSegment::Name(name) => name.clone(),
    }
}

/// A saved key as plain text for a child label.
fn key_text(key: &SavedKey) -> String {
    match key {
        SavedKey::Int(value) => value.to_string(),
        SavedKey::Bool(value) => value.to_string(),
        SavedKey::Str(value) => format!("\"{value}\""),
        SavedKey::Bytes(bytes) => hex(bytes),
        SavedKey::Date(value) => encoded_key(Scalar::Date(*value)),
        SavedKey::Duration(value) => encoded_key(Scalar::Duration(*value)),
        SavedKey::Instant(value) => encoded_key(Scalar::Instant(*value)),
    }
}

/// The canonical text of a date/duration/instant key.
fn encoded_key(scalar: Scalar) -> String {
    match encode_value(&scalar) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => "<unrepresentable>".to_string(),
    }
}

/// Resolve a watch/REPL `^`-path expression string to its saved-path segments,
/// typing keys from the program's schema, then read the leaf value there.
/// Returns `Err` with a short reason when the text is not a resolvable read-only
/// `^` path (the only watchable shape), or `Ok(None)` when it resolves but no
/// value is stored at that path.
pub fn watch(
    store: &dyn Backend,
    program: &CheckedProgram,
    expression: &str,
) -> Result<Option<String>, String> {
    let path = resolve_caret_path(program, expression)
        .ok_or_else(|| "only ^ paths are watchable".to_string())?;
    Ok(read_leaf(store, program, &path))
}

/// Parse a bare `^`-path expression and resolve it to saved-path segments using
/// the existing core resolver, which types keys from the schema. The expression
/// is wrapped in a throwaway module so the real parser handles it — no
/// hand-rolled path parsing, so it tracks `^`-path syntax exactly.
fn resolve_caret_path(program: &CheckedProgram, expression: &str) -> Option<Vec<PathSegment>> {
    let trimmed = expression.trim();
    if !trimmed.starts_with('^') {
        return None;
    }
    let prefix = "module __watch\n\npub fn __watch()\n    const __x = ";
    let source = format!("{prefix}{trimmed}\n");
    let parsed = marrow_syntax::parse_source(&source);
    let offset = prefix.len();
    marrow_lsp_core::store::saved_path_at(&parsed.file, program, offset)
}

/// Encode a scalar to its stored bytes, for tests that seed a store with typed
/// leaves matching the schema.
#[cfg(test)]
pub(crate) fn scalar_bytes(scalar: &Scalar) -> Vec<u8> {
    encode_value(scalar).expect("a test scalar encodes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_check::{ProjectSources, analyze_project};
    use marrow_project::ProjectConfig;
    use marrow_store::mem::MemStore;
    use std::cell::RefCell;

    const SCHEMA: &str = "module shelf\n\
                          \n\
                          resource Book at ^books(id: int)\n\
                          \x20   required title: string\n\
                          \x20   required pages: int\n\
                          \n\
                          pub fn touch(): int\n\
                          \x20   return 1\n";

    fn program() -> CheckedProgram {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("shelf.mw"), SCHEMA).unwrap();
        let config = ProjectConfig {
            source_roots: vec!["src".to_string()],
            default_entry: None,
            store: None,
            tests: Vec::new(),
        };
        analyze_project(root, &config, &ProjectSources::new())
            .unwrap()
            .program
    }

    /// Seed a store: `^books(1).title = "Dune"`, `^books(1).pages = 412`.
    fn seeded() -> RefCell<MemStore> {
        let store = RefCell::new(MemStore::new());
        {
            let mut backend = store.borrow_mut();
            let title = vec![
                PathSegment::Root("books".to_string()),
                PathSegment::RecordKey(SavedKey::Int(1)),
                PathSegment::Field("title".to_string()),
            ];
            backend.write(
                &encode_path(&title),
                scalar_bytes(&Scalar::Str("Dune".into())),
            );
            let pages = vec![
                PathSegment::Root("books".to_string()),
                PathSegment::RecordKey(SavedKey::Int(1)),
                PathSegment::Field("pages".to_string()),
            ];
            backend.write(&encode_path(&pages), scalar_bytes(&Scalar::Int(412)));
        }
        store
    }

    #[test]
    fn roots_lists_seeded_roots_as_caret_nodes() {
        let store = seeded();
        let nodes = roots(&*store.borrow());
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "^books");
        assert!(nodes[0].expandable);
    }

    #[test]
    fn children_walk_records_then_fields_with_typed_values() {
        let program = program();
        let store = seeded();
        let backend = store.borrow();
        // ^books -> the record (1)
        let records = children(&*backend, &program, &[PathSegment::Root("books".into())]);
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].name, "(1)");
        // ^books(1) -> title, pages
        let fields = children(&*backend, &program, &records[0].path);
        let title = fields.iter().find(|n| n.name == "title").unwrap();
        assert_eq!(title.value, "\"Dune\"");
        let pages = fields.iter().find(|n| n.name == "pages").unwrap();
        assert_eq!(pages.value, "412");
    }

    #[test]
    fn render_decodes_a_typed_reference_leaf_to_an_identity() {
        let bytes = [
            marrow_store::path::encode_key_value(&SavedKey::Str("Ada\nLovelace".to_string())),
            marrow_store::path::encode_key_value(&SavedKey::Bytes(vec![0x0a, 0xff])),
            marrow_store::path::encode_key_value(&SavedKey::Date(0)),
        ]
        .concat();

        let rendered = render(
            &bytes,
            SavedPathClass::Identity {
                resource: "Author".to_string(),
                arity: 3,
            },
        );

        assert_eq!(
            rendered,
            "Author::Id(\"Ada\\nLovelace\", 0x0aff, 1970-01-01)"
        );
    }

    #[test]
    fn watch_resolves_a_caret_path_to_its_typed_leaf() {
        let program = program();
        let store = seeded();
        let backend = store.borrow();
        assert_eq!(
            watch(&*backend, &program, "^books(1).title"),
            Ok(Some("\"Dune\"".to_string()))
        );
        assert_eq!(
            watch(&*backend, &program, "^books(1).pages"),
            Ok(Some("412".to_string()))
        );
    }

    #[test]
    fn watch_reads_your_own_uncommitted_writes() {
        let program = program();
        let store = RefCell::new(MemStore::new());
        {
            let mut backend = store.borrow_mut();
            Backend::begin(&mut *backend).unwrap();
            let pages = vec![
                PathSegment::Root("books".to_string()),
                PathSegment::RecordKey(SavedKey::Int(7)),
                PathSegment::Field("pages".to_string()),
            ];
            backend.write(&encode_path(&pages), scalar_bytes(&Scalar::Int(99)));
            // No commit: the write lives only in the open savepoint.
        }
        let backend = store.borrow();
        assert_eq!(
            watch(&*backend, &program, "^books(7).pages"),
            Ok(Some("99".to_string()))
        );
    }

    #[test]
    fn a_non_caret_expression_is_not_watchable() {
        let program = program();
        let store = seeded();
        let backend = store.borrow();
        assert_eq!(
            watch(&*backend, &program, "title"),
            Err("only ^ paths are watchable".to_string())
        );
    }

    #[test]
    fn watch_on_an_absent_path_resolves_to_no_value() {
        let program = program();
        let store = seeded();
        let backend = store.borrow();
        assert_eq!(watch(&*backend, &program, "^books(2).title"), Ok(None));
    }
}
