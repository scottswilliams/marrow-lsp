//! A read-only, schema-typed reader over a project's durable saved tree.
//!
//! Editor features (live hover values, the record-count lens, the Data Explorer)
//! inspect a project's stored data. They must never write it and must never block
//! the editor, so every operation here opens the store read-only, performs one
//! short, bounded read, and drops the handle. A store that is locked by a running
//! writer, corrupt, or written in an unsupported format is reported as a soft
//! [`Availability::Unavailable`] — never an error surfaced to the user.
//!
//! The reader resolves the store path from a [`Project`]: the native backend
//! keeps its file at `<root>/<data_dir>/marrow.redb`; a memory backend, or a
//! project that pins no store, has no inspectable live data.

use std::path::PathBuf;

use marrow_check::CheckedProgram;
use marrow_project::{StoreBackend, StoreConfig};
use marrow_run::{SavedPathClass, classify_saved_path};
use marrow_store::backend::{Backend, Presence, StoreError};
use marrow_store::path::{ChildSegment, PathSegment, SavedKey, encode_path};
use marrow_store::redb::RedbStore;
use marrow_store::value::{Scalar, decode_value};

use crate::workspace::Project;

/// The most child entries one [`children`] call returns. The Data Explorer pages
/// a tree node at a time; a node with more children than this is reported with a
/// `more` marker rather than materializing an unbounded list.
const CHILD_CAP: usize = 200;

/// The most record keys [`record_count`] enumerates before reporting an
/// approximate `>= N`. A small root returns its exact count; a large one is
/// sampled so a count never walks an unbounded band.
const COUNT_CAP: usize = 1000;

/// The file name of a native store inside its data directory.
const STORE_FILE: &str = "marrow.redb";

/// A read accessor over a project's saved tree, available only when the project
/// pins a native store. Each method opens the store read-only, reads, and drops
/// the handle, so the reader holds nothing open between calls and a writer can
/// always take the file.
pub struct StoreReader {
    path: PathBuf,
}

impl StoreReader {
    /// The reader for `project`, or `None` when the project has no inspectable
    /// live store: a memory backend (no on-disk file) or no store pinned at all.
    /// A `None` here means "no live data", distinct from an [`Availability::Unavailable`]
    /// store that exists but cannot be read right now.
    pub fn for_project(project: &Project) -> Option<Self> {
        let StoreConfig { backend, data_dir } = project.config.store.as_ref()?;
        match backend {
            StoreBackend::Native => {
                // A native store's config is validated to carry a non-empty data
                // dir at parse time, so this join always lands on a real path.
                let dir = data_dir.as_deref()?;
                Some(Self {
                    path: project.root.join(dir).join(STORE_FILE),
                })
            }
            StoreBackend::Memory => None,
        }
    }

    /// The saved root names, in Marrow order, or [`Availability::Unavailable`]
    /// when the store cannot be read right now.
    pub fn roots(&self) -> Availability<Vec<String>> {
        self.with_store(|store| store.roots().ok())
    }

    /// The immediate children of `path`, capped at [`CHILD_CAP`]. The `more` flag
    /// is set when the store held additional children past the cap. An unreadable
    /// store yields [`Availability::Unavailable`].
    pub fn children(&self, path: &[PathSegment]) -> Availability<Children> {
        let key = encode_path(path);
        self.with_store(|store| {
            let mut keys = store.child_keys(&key).ok()?;
            let more = keys.len() > CHILD_CAP;
            keys.truncate(CHILD_CAP);
            Some(Children {
                entries: keys,
                more,
            })
        })
    }

    /// The presence at `path` and, when a value is stored there, its rendered
    /// form. The path is classified against `program`'s schema so a scalar leaf
    /// decodes to a human string of its declared type; a value that does not
    /// decode renders as raw hex so it stays inspectable. An unreadable store
    /// yields [`Availability::Unavailable`].
    pub fn get(&self, path: &[PathSegment], program: &CheckedProgram) -> Availability<StoredValue> {
        let key = encode_path(path);
        let class = classify_saved_path(program, path);
        self.with_store(|store| {
            let presence = store.presence(&key).ok()?;
            let value = store
                .read(&key)
                .ok()?
                .map(|bytes| render_value(&bytes, class));
            Some(StoredValue { presence, value })
        })
    }

    /// An exact small count of the records under `^root`, or a sampled `>= N`
    /// floor once the count reaches [`COUNT_CAP`]. For an integer-keyed root the
    /// store can hint the highest key cheaply, but the displayed count is still
    /// the number of records present (records may have been deleted), so the
    /// count enumerates child record keys up to the cap. An unreadable store
    /// yields [`Availability::Unavailable`].
    pub fn record_count(&self, root: &str) -> Availability<RecordCount> {
        let key = encode_path(&[PathSegment::Root(root.to_string())]);
        self.with_store(|store| {
            let children = store.child_keys(&key).ok()?;
            // Only record keys count as records; named members directly under a
            // root are indexes, never identities.
            let records = children
                .iter()
                .filter(|child| matches!(child, ChildSegment::Key(_)))
                .count();
            Some(if records >= COUNT_CAP {
                RecordCount::AtLeast(records)
            } else {
                RecordCount::Exact(records)
            })
        })
    }

    /// Open the store read-only, run `read`, and drop the handle. A missing file,
    /// a lock held by a live writer, a corrupt store, an unsupported format
    /// version, or a read that fails partway all collapse to
    /// [`Availability::Unavailable`]: inspection is best-effort and never an error.
    fn with_store<T>(&self, read: impl FnOnce(&ReadOnly) -> Option<T>) -> Availability<T> {
        match RedbStore::open_read_only(&self.path) {
            Ok(store) => match read(&ReadOnly(store)) {
                Some(value) => Availability::Available(value),
                None => Availability::Unavailable,
            },
            // Locked/corrupt/format/missing all degrade to unavailable, never an
            // error: a running app holds the writer lock often, and that is normal.
            Err(_) => Availability::Unavailable,
        }
    }
}

/// A store wrapper exposing only the [`Backend`] read methods. `open_read_only`
/// returns a technically-writable handle (redb has no read-only database), so
/// this newtype makes the write, delete, and transaction methods unreachable:
/// the inspection surface cannot mutate managed data even by mistake.
struct ReadOnly(RedbStore);

impl ReadOnly {
    fn read(&self, path: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.0.read(path)
    }

    fn presence(&self, path: &[u8]) -> Result<Presence, StoreError> {
        self.0.presence(path)
    }

    fn child_keys(&self, path: &[u8]) -> Result<Vec<ChildSegment>, StoreError> {
        self.0.child_keys(path)
    }

    fn roots(&self) -> Result<Vec<String>, StoreError> {
        self.0.roots()
    }
}

/// Whether an inspection could read the store. A store that exists but is locked,
/// corrupt, or of an unsupported format reads as `Unavailable` rather than
/// raising an error, so a transient condition never breaks an editor feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability<T> {
    Available(T),
    Unavailable,
}

impl<T> Availability<T> {
    /// The value when available, else `None`.
    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Available(value) => Some(value),
            Self::Unavailable => None,
        }
    }
}

/// The immediate children of a path, with `more` set when the store held entries
/// past the cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Children {
    pub entries: Vec<ChildSegment>,
    pub more: bool,
}

/// The presence at a saved path plus its rendered value, if a value is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredValue {
    pub presence: Presence,
    /// The rendered value at this exact path, or `None` when the path holds only
    /// children (a record node) or nothing.
    pub value: Option<String>,
}

/// A record count for a saved root: exact for a small root, an approximate floor
/// once the count reaches the cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordCount {
    Exact(usize),
    AtLeast(usize),
}

impl RecordCount {
    /// The count as display text: `3 records`, or `>= 1000 records` when sampled.
    /// Singular `1 record` reads naturally for a one-record root.
    pub fn display(self) -> String {
        match self {
            Self::Exact(1) => "1 record".to_string(),
            Self::Exact(n) => format!("{n} records"),
            Self::AtLeast(n) => format!(">= {n} records"),
        }
    }
}

/// Render stored bytes to a human string using the path's schema classification.
/// A declared scalar leaf decodes to a canonical form of its type; an index
/// marker or a value that does not decode renders as raw hex so corrupt or
/// untyped data stays visible rather than vanishing.
fn render_value(bytes: &[u8], class: SavedPathClass) -> String {
    match class {
        SavedPathClass::Scalar(ty) => match decode_value(bytes, ty) {
            Some(scalar) => render_scalar(&scalar),
            None => hex(bytes),
        },
        // An index marker holds a presence marker or a raw identity, not a typed
        // scalar; an orphan is data the schema does not account for. Both are
        // shown raw rather than guessed at.
        SavedPathClass::IndexMarker | SavedPathClass::Orphan => hex(bytes),
    }
}

/// A decoded scalar as the text a developer reads. Strings show with quotes so an
/// empty or whitespace value is unambiguous; every other scalar shows its
/// canonical Marrow spelling.
fn render_scalar(scalar: &Scalar) -> String {
    match scalar {
        Scalar::Bool(value) => value.to_string(),
        Scalar::Int(value) => value.to_string(),
        Scalar::Str(text) => format!("\"{text}\""),
        Scalar::Bytes(bytes) => hex(bytes),
        Scalar::Date(_) | Scalar::Duration(_) | Scalar::Instant(_) => canonical(scalar),
        Scalar::Decimal(value) => value.to_text(),
    }
}

/// The canonical encoded spelling of a date, duration, or instant — the same form
/// the store holds and the CLI prints. These have no simpler human form than
/// their canonical text, so re-encoding is the faithful rendering.
fn canonical(scalar: &Scalar) -> String {
    match marrow_store::value::encode_value(scalar) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        // A date/instant outside the representable year range cannot re-encode;
        // it is not reachable from a value the store accepted, but stay total.
        Err(_) => "<unrepresentable>".to_string(),
    }
}

/// Lowercase hex of raw bytes, the fallback rendering for an undecodable or
/// untyped value.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The key a record-key child segment carries as a [`SavedKey`], for callers that
/// build a child path. A named child carries no key.
pub fn child_key(child: &ChildSegment) -> Option<&SavedKey> {
    match child {
        ChildSegment::Key(key) => Some(key),
        ChildSegment::Name(_) => None,
    }
}

/// Resolve the saved-path expression covering byte `offset` to its path segments,
/// or `None` when the cursor is not on a saved path. The whole chain rooted at
/// the cursor's `^root` is resolved — `^books(1).title` yields the three segments
/// regardless of where in it the cursor sits — so a hover anywhere on the path
/// reads the same stored leaf.
///
/// Keys are typed from `program`'s schema (a saved root's identity key types and
/// a keyed layer's key params), so an integer literal under an `int` identity
/// becomes [`SavedKey::Int`]. A key whose literal does not match a known key type,
/// or a dynamic (non-literal) key, makes the path unresolvable rather than guessed.
pub fn saved_path_at(
    file: &marrow_syntax::SourceFile,
    program: &CheckedProgram,
    offset: usize,
) -> Option<Vec<PathSegment>> {
    let expr = find_saved_path_expr(file, offset)?;
    resolve_path_expr(expr, program)
}

/// The outermost saved-path expression whose span covers `offset`: an expression
/// rooted at a `^root` and built only of key-lookup calls and field accesses. The
/// outermost match is returned so the full path (not just the inner `^root`)
/// resolves.
fn find_saved_path_expr(
    file: &marrow_syntax::SourceFile,
    offset: usize,
) -> Option<&marrow_syntax::Expression> {
    let mut found: Option<&marrow_syntax::Expression> = None;
    for declaration in &file.declarations {
        walk_declaration(declaration, offset, &mut found);
    }
    found
}

fn walk_declaration<'a>(
    declaration: &'a marrow_syntax::Declaration,
    offset: usize,
    found: &mut Option<&'a marrow_syntax::Expression>,
) {
    if let marrow_syntax::Declaration::Function(function) = declaration {
        walk_block(&function.body, offset, found);
    }
}

fn walk_block<'a>(
    block: &'a marrow_syntax::Block,
    offset: usize,
    found: &mut Option<&'a marrow_syntax::Expression>,
) {
    for statement in &block.statements {
        walk_statement(statement, offset, found);
    }
}

fn walk_statement<'a>(
    statement: &'a marrow_syntax::Statement,
    offset: usize,
    found: &mut Option<&'a marrow_syntax::Expression>,
) {
    use marrow_syntax::Statement;
    match statement {
        Statement::Const { value, .. } | Statement::Throw { value, .. } => {
            walk_expr(value, offset, found)
        }
        Statement::Var { value, .. } => {
            if let Some(value) = value {
                walk_expr(value, offset, found)
            }
        }
        Statement::Assign { target, value, .. } | Statement::Merge { target, value, .. } => {
            walk_expr(target, offset, found);
            walk_expr(value, offset, found);
        }
        Statement::Delete { path, .. } => walk_expr(path, offset, found),
        Statement::Return { value, .. } => {
            if let Some(value) = value {
                walk_expr(value, offset, found)
            }
        }
        Statement::Expr { value, .. } => walk_expr(value, offset, found),
        Statement::If {
            condition,
            then_block,
            else_ifs,
            else_block,
            ..
        } => {
            if let Some(condition) = condition {
                walk_expr(condition, offset, found)
            }
            walk_block(then_block, offset, found);
            for else_if in else_ifs {
                if let Some(condition) = &else_if.condition {
                    walk_expr(condition, offset, found)
                }
                walk_block(&else_if.block, offset, found);
            }
            if let Some(else_block) = else_block {
                walk_block(else_block, offset, found)
            }
        }
        Statement::While {
            condition, body, ..
        } => {
            if let Some(condition) = condition {
                walk_expr(condition, offset, found)
            }
            walk_block(body, offset, found);
        }
        Statement::For { iterable, body, .. } => {
            walk_expr(iterable, offset, found);
            walk_block(body, offset, found);
        }
        Statement::Transaction { body, .. } => walk_block(body, offset, found),
        Statement::Lock { path, body, .. } => {
            if let Some(path) = path {
                walk_expr(path, offset, found)
            }
            walk_block(body, offset, found);
        }
        Statement::Try {
            body,
            catch,
            finally,
            ..
        } => {
            walk_block(body, offset, found);
            if let Some(catch) = catch {
                walk_block(&catch.block, offset, found)
            }
            if let Some(finally) = finally {
                walk_block(finally, offset, found)
            }
        }
        Statement::Break { .. } | Statement::Continue { .. } => {}
    }
}

/// Record `expr` if it is a saved path covering `offset`, else descend into its
/// subexpressions. A saved-path expression is recorded as a whole — its inner
/// `^root` and key arguments are not descended into separately — so the outermost
/// path wins.
fn walk_expr<'a>(
    expr: &'a marrow_syntax::Expression,
    offset: usize,
    found: &mut Option<&'a marrow_syntax::Expression>,
) {
    let span = expr.span();
    let covers = offset >= span.start_byte && offset <= span.end_byte;
    if covers && is_saved_path(expr) {
        *found = Some(expr);
        return;
    }
    use marrow_syntax::Expression;
    match expr {
        Expression::Call { callee, args, .. } => {
            walk_expr(callee, offset, found);
            for arg in args {
                walk_expr(&arg.value, offset, found);
            }
        }
        Expression::Field { base, .. } | Expression::OptionalField { base, .. } => {
            walk_expr(base, offset, found)
        }
        Expression::Unary { operand, .. } => walk_expr(operand, offset, found),
        Expression::Binary { left, right, .. } => {
            walk_expr(left, offset, found);
            walk_expr(right, offset, found);
        }
        Expression::Interpolation { parts, .. } => {
            for part in parts {
                if let marrow_syntax::InterpolationPart::Expr(inner) = part {
                    walk_expr(inner, offset, found)
                }
            }
        }
        Expression::Literal { .. } | Expression::Name { .. } | Expression::SavedRoot { .. } => {}
    }
}

/// Whether `expr` is a saved-path expression: a `^root` with any chain of field
/// accesses and key-lookup calls on top.
fn is_saved_path(expr: &marrow_syntax::Expression) -> bool {
    use marrow_syntax::Expression;
    match expr {
        Expression::SavedRoot { .. } => true,
        Expression::Field { base, .. } | Expression::OptionalField { base, .. } => {
            is_saved_path(base)
        }
        Expression::Call { callee, .. } => is_saved_path(callee),
        _ => false,
    }
}

/// Resolve a saved-path expression to its [`PathSegment`]s, typing every key from
/// the schema. `None` when a key is dynamic or its literal does not match the
/// declared key type, since a typed read needs concrete keys.
fn resolve_path_expr(
    expr: &marrow_syntax::Expression,
    program: &CheckedProgram,
) -> Option<Vec<PathSegment>> {
    use marrow_syntax::Expression;
    match expr {
        Expression::SavedRoot { name, .. } => Some(vec![PathSegment::Root(name.clone())]),
        Expression::Field { base, name, .. } | Expression::OptionalField { base, name, .. } => {
            let mut segments = resolve_path_expr(base, program)?;
            segments.push(PathSegment::Field(name.clone()));
            Some(segments)
        }
        Expression::Call { callee, args, .. } => {
            let mut segments = resolve_path_expr(callee, program)?;
            // The key types for this call position: a call directly on `^root`
            // carries the identity keys; a call on a field (a keyed layer) carries
            // that layer's key params. Resolution without the schema position
            // would mis-type, so an unknown position fails.
            let key_types = call_key_types(callee, program)?;
            if args.len() != key_types.len() {
                return None;
            }
            // A call directly on a root carries identity record keys; a call on a
            // keyed layer carries index keys.
            let on_root = matches!(callee.as_ref(), Expression::SavedRoot { .. });
            for (arg, ty) in args.iter().zip(key_types) {
                if arg.name.is_some() || arg.mode.is_some() {
                    return None;
                }
                let key = literal_key(&arg.value, ty)?;
                segments.push(if on_root {
                    PathSegment::RecordKey(key)
                } else {
                    PathSegment::IndexKey(key)
                });
            }
            Some(segments)
        }
        _ => None,
    }
}

/// The declared key types for a key-lookup call whose callee is `callee`: the
/// identity key types when the callee is a `^root`, or the keyed layer's key
/// params when it is a field on a saved path. `None` when the position carries no
/// keys (so a call there is not a key lookup) or the root is unknown.
fn call_key_types(
    callee: &marrow_syntax::Expression,
    program: &CheckedProgram,
) -> Option<Vec<marrow_schema::ScalarType>> {
    use marrow_syntax::Expression;
    match callee {
        Expression::SavedRoot { name, .. } => {
            let resource = resource_by_root(program, name)?;
            let saved = resource.saved_root.as_ref()?;
            saved
                .identity_keys
                .iter()
                .map(|key| key.ty.scalar())
                .collect()
        }
        // A keyed layer `^root(id…).layer(key…)`: walk the schema to the layer
        // named by the field chain and read its key params.
        Expression::Field { base, name, .. } | Expression::OptionalField { base, name, .. } => {
            let root = saved_root_name(base)?;
            let resource = resource_by_root(program, root)?;
            let layers = field_chain(base)?;
            let mut chain: Vec<&str> = layers.iter().map(String::as_str).collect();
            chain.push(name);
            let node = resource.descend_layers(&chain)?;
            node.key_params.iter().map(|key| key.ty.scalar()).collect()
        }
        _ => None,
    }
}

/// The saved root name at the base of a saved-path expression, if any.
fn saved_root_name(expr: &marrow_syntax::Expression) -> Option<&str> {
    use marrow_syntax::Expression;
    match expr {
        Expression::SavedRoot { name, .. } => Some(name),
        Expression::Field { base, .. } | Expression::OptionalField { base, .. } => {
            saved_root_name(base)
        }
        Expression::Call { callee, .. } => saved_root_name(callee),
        _ => None,
    }
}

/// The field names along a saved-path expression, outermost last, skipping the
/// identity key-lookup calls. For `^root(id).a.b` this is `["a", "b"]`.
fn field_chain(expr: &marrow_syntax::Expression) -> Option<Vec<String>> {
    use marrow_syntax::Expression;
    match expr {
        Expression::SavedRoot { .. } => Some(Vec::new()),
        Expression::Field { base, name, .. } | Expression::OptionalField { base, name, .. } => {
            let mut chain = field_chain(base)?;
            chain.push(name.clone());
            Some(chain)
        }
        Expression::Call { callee, .. } => field_chain(callee),
        _ => None,
    }
}

/// A literal expression as a [`SavedKey`] of scalar type `ty`, or `None` when the
/// expression is not a literal of that type. Handles the key-able scalars an
/// editor path commonly carries (int, string, bool); a non-literal or mismatched
/// type makes the path unresolvable.
fn literal_key(
    expr: &marrow_syntax::Expression,
    ty: marrow_schema::ScalarType,
) -> Option<SavedKey> {
    use marrow_schema::ScalarType;
    use marrow_syntax::{Expression, LiteralKind};
    let Expression::Literal { kind, text, .. } = expr else {
        return None;
    };
    match (ty, kind) {
        (ScalarType::Int, LiteralKind::Integer) => text.parse().ok().map(SavedKey::Int),
        (ScalarType::Str, LiteralKind::String) => Some(SavedKey::Str(text.clone())),
        (ScalarType::Bool, LiteralKind::Bool) => match text.as_str() {
            "true" => Some(SavedKey::Bool(true)),
            "false" => Some(SavedKey::Bool(false)),
            _ => None,
        },
        _ => None,
    }
}

/// The resource whose saved root is `root`, if the program declares one.
fn resource_by_root<'a>(
    program: &'a CheckedProgram,
    root: &str,
) -> Option<&'a marrow_schema::ResourceSchema> {
    program
        .modules
        .iter()
        .flat_map(|module| &module.resources)
        .find(|resource| {
            resource
                .saved_root
                .as_ref()
                .is_some_and(|saved| saved.root == root)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_check::{ProjectSources, analyze_project};
    use marrow_project::parse_config;
    use marrow_store::path::{PathSegment, SavedKey, encode_path};
    use marrow_store::redb::RedbStore;

    /// A project whose `marrow.json` pins a native store under `data/`, with one
    /// `Book` resource saved at `^books`. Returns the resolved project, its
    /// checked program, and the path the native store file will live at.
    fn project_with_store() -> (Project, CheckedProgram, PathBuf) {
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
        let source = "\
module shelf

resource Book at ^books(id: int)
    required title: string
";
        std::fs::write(src.join("shelf.mw"), source).unwrap();

        let config = parse_config(config_text).unwrap();
        let snapshot = analyze_project(&root, &config, &ProjectSources::new()).unwrap();
        let project = Project {
            root: root.clone(),
            config,
        };
        let store_path = root.join("data").join(STORE_FILE);
        // Keep the temp dir alive for the test's lifetime; the OS reclaims it on
        // process exit.
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

    /// Write `^books(1).title = "Mort"` into a fresh native store at `path`.
    fn seed_one_book(path: &std::path::Path) {
        let mut store = RedbStore::open(path).unwrap();
        store
            .write(&book_title(1), b"Mort".to_vec())
            .expect("write the title");
        // The store drops here, releasing the writer lock so a read-only open can
        // take the file.
    }

    #[test]
    fn roots_lists_the_saved_root() {
        let (project, _program, path) = project_with_store();
        seed_one_book(&path);
        let reader = StoreReader::for_project(&project).expect("native store has a reader");
        let roots = reader.roots().into_option().expect("store is readable");
        assert_eq!(roots, vec!["books".to_string()]);
    }

    #[test]
    fn children_of_the_root_are_the_record_keys() {
        let (project, _program, path) = project_with_store();
        seed_one_book(&path);
        let reader = StoreReader::for_project(&project).unwrap();
        let children = reader
            .children(&[PathSegment::Root("books".to_string())])
            .into_option()
            .expect("readable");
        assert!(!children.more, "one book is well under the cap");
        assert_eq!(children.entries, vec![ChildSegment::Key(SavedKey::Int(1))]);
    }

    #[test]
    fn get_decodes_a_leaf_to_its_scalar_type() {
        let (project, program, path) = project_with_store();
        seed_one_book(&path);
        let reader = StoreReader::for_project(&project).unwrap();
        let stored = reader
            .get(
                &[
                    PathSegment::Root("books".to_string()),
                    PathSegment::RecordKey(SavedKey::Int(1)),
                    PathSegment::Field("title".to_string()),
                ],
                &program,
            )
            .into_option()
            .expect("readable");
        assert_eq!(stored.presence, Presence::ValueOnly);
        // `title` is a `string` field, so the bytes decode and render quoted.
        assert_eq!(stored.value.as_deref(), Some("\"Mort\""));
    }

    #[test]
    fn get_of_a_record_node_is_children_only_with_no_value() {
        let (project, program, path) = project_with_store();
        seed_one_book(&path);
        let reader = StoreReader::for_project(&project).unwrap();
        let stored = reader
            .get(
                &[
                    PathSegment::Root("books".to_string()),
                    PathSegment::RecordKey(SavedKey::Int(1)),
                ],
                &program,
            )
            .into_option()
            .expect("readable");
        assert_eq!(stored.presence, Presence::ChildrenOnly);
        assert_eq!(stored.value, None);
    }

    #[test]
    fn record_count_is_exact_for_a_small_root() {
        let (project, _program, path) = project_with_store();
        // Two books.
        {
            let mut store = RedbStore::open(&path).unwrap();
            store.write(&book_title(1), b"Mort".to_vec()).unwrap();
            store.write(&book_title(2), b"Sourcery".to_vec()).unwrap();
        }
        let reader = StoreReader::for_project(&project).unwrap();
        let count = reader
            .record_count("books")
            .into_option()
            .expect("readable");
        assert_eq!(count, RecordCount::Exact(2));
        assert_eq!(count.display(), "2 records");
    }

    #[test]
    fn a_writer_holding_the_file_degrades_to_unavailable() {
        let (project, program, path) = project_with_store();
        seed_one_book(&path);
        // A live writer holds the redb file lock. A read-only open must not error
        // or block: it degrades to Unavailable.
        let _writer = RedbStore::open(&path).expect("open a writer");
        let reader = StoreReader::for_project(&project).unwrap();
        assert_eq!(reader.roots(), Availability::Unavailable);
        assert_eq!(
            reader.children(&[PathSegment::Root("books".to_string())]),
            Availability::Unavailable
        );
        assert_eq!(
            reader.get(
                &[
                    PathSegment::Root("books".to_string()),
                    PathSegment::RecordKey(SavedKey::Int(1)),
                    PathSegment::Field("title".to_string()),
                ],
                &program,
            ),
            Availability::Unavailable
        );
        assert_eq!(reader.record_count("books"), Availability::Unavailable);
    }

    #[test]
    fn a_missing_store_file_is_unavailable_not_an_error() {
        // The project pins a native store, but no file was ever created.
        let (project, _program, _path) = project_with_store();
        let reader = StoreReader::for_project(&project).unwrap();
        assert_eq!(reader.roots(), Availability::Unavailable);
    }

    #[test]
    fn a_memory_backend_has_no_reader() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let config_text = r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#;
        let config = parse_config(config_text).unwrap();
        let project = Project { root, config };
        assert!(StoreReader::for_project(&project).is_none());
    }

    /// Parse and check a one-module source so a test can resolve a path against
    /// its schema, returning the program and the parsed source file.
    fn check_source(source: &str) -> (CheckedProgram, marrow_syntax::SourceFile) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("marrow.json"), r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("shelf.mw"), source).unwrap();
        let config = parse_config(r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let snapshot = analyze_project(root, &config, &ProjectSources::new()).unwrap();
        let file = marrow_syntax::parse_source(source).file;
        std::mem::forget(dir);
        (snapshot.program, file)
    }

    #[test]
    fn saved_path_at_resolves_a_literal_keyed_field() {
        let source = "\
module shelf

resource Book at ^books(id: int)
    required title: string

pub fn f(): string
    return ^books(1).title
";
        let (program, file) = check_source(source);
        // Cursor on `title` in `^books(1).title`.
        let offset = source.find(").title").unwrap() + ").".len() + 1;
        let segments = saved_path_at(&file, &program, offset).expect("a saved path");
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
    fn saved_path_at_resolves_from_the_caret() {
        let source = "\
module shelf

resource Book at ^books(id: int)
    required title: string

pub fn f(): string
    return ^books(1).title
";
        let (program, file) = check_source(source);
        // Cursor on the `^` in the function body resolves the whole path, not
        // just the root. (The resource decl also writes `^books`; the body's is
        // the last occurrence.)
        let offset = source.rfind("^books").unwrap();
        let segments = saved_path_at(&file, &program, offset).expect("a saved path");
        assert_eq!(segments.len(), 3, "the whole path resolves from the caret");
    }

    #[test]
    fn saved_path_off_any_path_is_none() {
        let source = "module shelf\n\npub fn f(): int\n    return 1\n";
        let (program, file) = check_source(source);
        let offset = source.find("return 1").unwrap() + "return ".len();
        assert!(saved_path_at(&file, &program, offset).is_none());
    }

    #[test]
    fn no_store_config_has_no_reader() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let config = parse_config(r#"{ "sourceRoots": ["src"] }"#).unwrap();
        let project = Project { root, config };
        assert!(StoreReader::for_project(&project).is_none());
    }
}
