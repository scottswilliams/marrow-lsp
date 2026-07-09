use super::*;

/// The read grant a data-tool test hands to a read tool, standing in for the
/// operator opt-in the transport would supply.
fn granted_read() -> Option<ReadAccess> {
    Some(ReadAccess::granted())
}

/// The write grant a surface-write test hands to [`surface_write`].
fn granted_write() -> Option<WriteAccess> {
    Some(WriteAccess::granted())
}

use marrow_check::test_support::{member_catalog_id, root_place, store_id_of};
use marrow_check::{
    CheckedProgram, check_project, check_project_against, project_store_lock, read_committed_lock,
};
use marrow_json::DATA_GENERATION_PROFILE_VERSION;
use marrow_project::{CATALOG_FILE_NAME, parse_config};
use marrow_store::key::SavedKey;
use marrow_store::tree::{DataPathSegment, StoreUid};
use marrow_store::value::{SavedValue, encode_value};
use marrow_store::{AccessMode, SealedStore};

fn assert_run_contract(result: &Json) {
    assert_production_contract(result, "sandboxed execution API");
}

fn assert_completion_contract(result: &Json) {
    assert_eq!(result["profile_version"], "source.completion.v1");
    assert_production_contract(result, "source completion fact");
    assert_eq!(result["contract"]["basis"], "Marrow source completion fact");
}

fn assert_production_contract(result: &Json, description: &str) {
    let contract = &result["contract"];
    assert_eq!(contract["status"], "ready", "contract status for {result}");
    assert_eq!(
        contract["stableProductionApi"], true,
        "stableProductionApi must be true for {result}"
    );
    assert_eq!(
        contract["description"], description,
        "contract description for {result}"
    );
    assert_eq!(
        contract["missingFacts"],
        json!([]),
        "production contract must not list missing facts for {result}"
    );
}

fn assert_saved_roots_contract(result: &Json) {
    assert_eq!(result["contract"], saved_roots_contract(), "{result}");
}

fn assert_data_children_contract(result: &Json) {
    assert_eq!(result["contract"], data_children_contract(), "{result}");
}

fn assert_data_read_contract(result: &Json) {
    assert_eq!(result["contract"], data_read_contract(), "{result}");
}

fn assert_store_snapshot(snapshot: &Json) {
    assert_eq!(
        snapshot["profile_version"], DATA_GENERATION_PROFILE_VERSION,
        "snapshot should carry the Marrow data generation profile version: {snapshot}"
    );
    assert_eq!(
        snapshot["store_uid"], "store_00000000000000000000000000000001",
        "snapshot should carry the fixture store UID: {snapshot}"
    );
    assert!(
        snapshot["catalog_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "snapshot should carry a catalog digest: {snapshot}"
    );
    let commit = &snapshot["commit"];
    assert!(
        commit["commit_id"].is_u64(),
        "snapshot should carry commit metadata: {snapshot}"
    );
    assert!(
        commit["catalog_epoch"].is_u64(),
        "snapshot commit should carry a catalog epoch: {snapshot}"
    );
    assert!(
        commit["source_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "snapshot commit should carry a source digest: {snapshot}"
    );
    assert!(
        commit["layout_epoch"].is_u64(),
        "snapshot commit should carry a layout epoch: {snapshot}"
    );
    assert!(
        commit["engine_profile_digest"]
            .as_str()
            .is_some_and(|digest| {
                digest.len() == 16
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            }),
        "snapshot commit should carry canonical profile digest hex: {snapshot}"
    );
    assert!(
        snapshot["checked_source_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "snapshot should carry the checked program source digest: {snapshot}"
    );
}

fn assert_data_view_boundary(boundary: &Json) {
    assert_eq!(
        boundary["sourceAnalysisGeneration"]["profileVersion"], "analysis.generation.v1",
        "boundary should carry Marrow source analysis generation: {boundary}"
    );
    assert_store_snapshot(&boundary["storeSnapshot"]);
    assert_eq!(
        boundary["compatibility"],
        json!({ "verdict": "admitted" }),
        "boundary should carry admitted compatibility: {boundary}"
    );
    assert!(
        boundary["watchTargets"]
            .as_array()
            .is_some_and(|targets| targets.len() == 2),
        "boundary should carry Marrow watch targets: {boundary}"
    );
}

/// Write a clean two-resource project to a temp dir and return its root and a
/// file inside it. The project declares a saved `Book` resource and a couple of
/// functions an `mw_run`/`mw_type_at` test can target.
fn project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" }, "tests": ["tests"] }"#,
        )
        .unwrap();
    let src = root.join("src/shelf");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("books.mw"),
        "\
module shelf::books

resource Book
    required title: string
    code: ErrorCode
    tags(pos: int): string

store ^books(id: int): Book
    index byTitle(title, id)

pub fn double(n: int): int
    return n * 2

pub fn greet(name: string): string
    return $\"hi {name}\"

pub fn shout()
    print(\"loud\")
",
    )
    .unwrap();
    let file = src.join("books.mw");
    (dir, file)
}

fn pure_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" }, "tests": ["tests"] }"#,
    )
    .unwrap();
    let src = root.join("src/shelf");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("books.mw"),
        "\
module shelf::books

pub fn double(n: int): int
    return n * 2

pub fn greet(name: string): string
    return $\"hi {name}\"

pub fn shout()
    print(\"loud\")

pub fn explode()
    print(\"before fault\")
    std::assert::isTrue(false)
",
    )
    .unwrap();
    let file = src.join("books.mw");
    (dir, file)
}

fn namespace_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" }, "tests": ["tests"] }"#,
    )
    .unwrap();
    let src = root.join("src/shelf");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("books.mw"),
        "\
module shelf::books

;; Books stored in the public shelf.
resource Book
    required title: string

store ^books(id: int): Book

;; Public reading status.
pub enum Status
    ;; Ready for use.
    active
    ;; Historical status.
    category archived
        ;; Out of print.
        retired

;; Resolves the display title for a book.
pub fn titleOf(
    ;; Book identity to resolve.
    id: Id(^books),
    ;; Title to use when the book is missing.
    fallback: string,
): string
    return fallback

;; Keeps a lifecycle state.
pub fn chooseStatus(label: string, state: Status): Status
    return state

pub fn shout()
    print(\"loud\")

fn privateTitle(): string
    return \"hidden\"
",
    )
    .unwrap();
    std::fs::write(
        src.join("app.mw"),
        "\
module shelf::app

use shelf::books

pub fn run(): int
    return 1
",
    )
    .unwrap();
    let file = src.join("app.mw");
    (dir, file)
}

fn namespace_project_with_local_alias_collision() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" }, "tests": ["tests"] }"#,
    )
    .unwrap();
    let src = root.join("src/shelf");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("books.mw"),
        "\
module shelf::books

resource Book
    required title: string

pub enum Status
    active

pub fn titleOf(): string
    return \"title\"
",
    )
    .unwrap();
    std::fs::write(
        src.join("app.mw"),
        "\
module shelf::app

use shelf::books

pub fn run(): int
    const books = 5
    return books
",
    )
    .unwrap();
    let file = src.join("app.mw");
    (dir, file)
}

fn namespace_project_with_evolve_alias_collision() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" }, "tests": ["tests"] }"#,
    )
    .unwrap();
    let src = root.join("src/shelf");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("books.mw"),
        "\
module shelf::books

resource Book
    required title: string

pub enum Status
    active

pub fn titleOf(): string
    return \"title\"
",
    )
    .unwrap();
    std::fs::write(
        src.join("app.mw"),
        "\
module shelf::app

use shelf::books

resource Draft
    required source: string
    title: string

store ^drafts(id: int): Draft

evolve
    transform Draft.title
        const books = old.source
        return books

pub fn run(): int
    return 1
",
    )
    .unwrap();
    let file = src.join("app.mw");
    (dir, file)
}

fn position_in_file(file: &Path, needle: &str) -> Position {
    let source = std::fs::read_to_string(file).unwrap();
    let offset = source
        .find(needle)
        .unwrap_or_else(|| panic!("fixture source should contain {needle:?}"));
    crate::positions::LineIndex::new(source).position(offset)
}

fn position_after_in_file(file: &Path, needle: &str) -> Position {
    let source = std::fs::read_to_string(file).unwrap();
    let offset = source
        .find(needle)
        .unwrap_or_else(|| panic!("fixture source should contain {needle:?}"))
        + needle.len();
    crate::positions::LineIndex::new(source).position(offset)
}

fn commit_project_lock(root: &Path) -> CheckedProgram {
    let config = parse_config(&std::fs::read_to_string(root.join("marrow.json")).unwrap()).unwrap();
    let (report, proposed) = check_project(root, &config).unwrap();
    assert!(!report.has_errors(), "{:#?}", report.diagnostics);
    let proposal = proposed.catalog.proposal.as_ref().unwrap();
    project_store_lock(root, proposal, &proposed.source_digest()).unwrap();
    let lock = read_committed_lock(root).unwrap();
    check_project_against(root, &config, None, lock.as_ref()).unwrap()
}

fn native_counter_project(ids: impl IntoIterator<Item = i64>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" }, "tests": ["tests"] }"#,
    )
    .unwrap();
    let src = root.join("src/app");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("counter.mw"),
        "\
module app::counter

resource Counter
    required value: int

store ^counter(id: int): Counter

pub fn show()
    if exists(^counter(1))
        print(\"present\")
        return
    print(\"absent\")
",
    )
    .unwrap();

    let program = commit_project_lock(root);
    let file = src.join("counter.mw");
    let place = root_place(&program, "counter").unwrap();
    let store_id = store_id_of(&place).unwrap();
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let store = SealedStore::open(&data_dir.join("marrow.redb"), AccessMode::Create)
        .unwrap()
        .into_store();
    store
        .write_store_uid(&StoreUid::new("store_00000000000000000000000000000001").unwrap())
        .unwrap();
    marrow_run::evolution::commit_catalog_baseline(&store, &program).unwrap();
    for id in ids {
        store
            .write_record_presence(&store_id, &[SavedKey::Int(id)])
            .unwrap();
    }
    drop(store);

    (dir, file)
}

fn native_counter_project_with_unstamped_records(
    ids: impl IntoIterator<Item = i64>,
) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" } }"#,
    )
    .unwrap();
    let src = root.join("src/app");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("counter.mw"),
        "\
module app::counter

resource Counter
    required value: int

store ^counter(id: int): Counter
",
    )
    .unwrap();

    let committed = commit_project_lock(root);

    let file = src.join("counter.mw");
    let place = root_place(&committed, "counter").unwrap();
    let store_id = store_id_of(&place).unwrap();
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let store = SealedStore::open(&data_dir.join("marrow.redb"), AccessMode::Create)
        .unwrap()
        .into_store();
    store
        .write_store_uid(&StoreUid::new("store_00000000000000000000000000000001").unwrap())
        .unwrap();
    for id in ids {
        store
            .write_record_presence(&store_id, &[SavedKey::Int(id)])
            .unwrap();
    }
    drop(store);

    (dir, file)
}

fn native_counter_project_with_value(value: i64) -> (tempfile::TempDir, PathBuf) {
    let (dir, file) = native_counter_project([1]);
    let root = dir.path();
    let program = with_loaded_project(&file, None, |workspace, _| {
        open_saved_data_session(workspace.project().unwrap())
            .unwrap()
            .unwrap()
            .surface_read()
            .program()
            .clone()
    })
    .unwrap();
    let place = root_place(&program, "counter").unwrap();
    let store_id = store_id_of(&place).unwrap();
    let value_id = member_catalog_id(&place, "value").unwrap();
    let path = vec![DataPathSegment::Member(
        marrow_store::cell::CatalogId::new(value_id).unwrap(),
    )];
    let store = SealedStore::open(&root.join("data/marrow.redb"), AccessMode::Create)
        .unwrap()
        .into_store();
    store
        .write_data_value(
            &store_id,
            &[SavedKey::Int(1)],
            &path,
            encode_value(&SavedValue::Int(value)).unwrap(),
        )
        .unwrap();
    drop(store);
    (dir, file)
}

fn native_settings_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" }, "tests": ["tests"] }"#,
    )
    .unwrap();
    let src = root.join("src/app");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("settings.mw"),
        "\
module app::settings

resource Settings
    required theme: string

store ^settings: Settings
",
    )
    .unwrap();

    let program = commit_project_lock(root);
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let store = SealedStore::open(&data_dir.join("marrow.redb"), AccessMode::Create)
        .unwrap()
        .into_store();
    store
        .write_store_uid(&StoreUid::new("store_00000000000000000000000000000001").unwrap())
        .unwrap();
    marrow_run::evolution::commit_catalog_baseline(&store, &program).unwrap();
    drop(store);

    (dir, src.join("settings.mw"))
}

fn saved_data_root_segment(file: &Path, root: &str) -> crate::data_explorer::DataPathSegmentDto {
    crate::data_explorer::DataPathSegmentDto::Root {
        store_catalog_id: saved_data_store_catalog_id(file, root),
    }
}

fn saved_data_field_segment(
    file: &Path,
    root: &str,
    member: &str,
) -> crate::data_explorer::DataPathSegmentDto {
    crate::data_explorer::DataPathSegmentDto::Field {
        member_catalog_id: saved_data_member_catalog_id(file, root, member),
    }
}

fn saved_data_store_catalog_id(file: &Path, root: &str) -> String {
    with_loaded_project(file, None, |workspace, _| {
        let session = open_saved_data_session(workspace.project().unwrap())
            .unwrap()
            .unwrap();
        let place = root_place(session.surface_read().program(), root).unwrap();
        store_id_of(&place).unwrap().as_str().to_string()
    })
    .unwrap()
}

fn saved_data_member_catalog_id(file: &Path, root: &str, member: &str) -> String {
    with_loaded_project(file, None, |workspace, _| {
        let session = open_saved_data_session(workspace.project().unwrap())
            .unwrap()
            .unwrap();
        let place = root_place(session.surface_read().program(), root).unwrap();
        member_catalog_id(&place, member).unwrap()
    })
    .unwrap()
}

fn sample_root_segment() -> crate::data_explorer::DataPathSegmentDto {
    crate::data_explorer::DataPathSegmentDto::Root {
        store_catalog_id: "cat_00000000000000000000000000000001".into(),
    }
}

fn write_surface_project(root: &Path) -> PathBuf {
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" } }"#,
    )
    .unwrap();
    let src = root.join("src/app");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("books.mw"),
        "\
module app::books

resource Book
    required title: string
    author: string

store ^books(id: int): Book
    index byAuthor(author, id)

surface Books from ^books
    fields title, author
    update author
    collection ^books.byAuthor as byAuthor
    action retitle

pub fn seed()
    var book: Book
    book.title = \"Dune\"
    book.author = \"Frank Herbert\"
    transaction
        ^books(1) = book

pub fn retitle(id: int, title: string): string
    transaction
        ^books(id).title = title
    return title
",
    )
    .unwrap();
    src.join("books.mw")
}

fn native_surface_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let file = write_surface_project(root);
    let session = ProjectSession::open(
        root,
        ProjectOpen::run().with_entry_override("app::books::seed".to_string()),
    )
    .expect("open seed session");
    let host = Host::new();
    let mut output = String::new();
    session
        .invoke(SessionEntry::new("app::books::seed", &host, &mut output))
        .expect("seed native surface project");
    assert_eq!(output, "");

    (dir, file)
}

fn catalog_only_surface_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let file = write_surface_project(dir.path());
    commit_project_lock(dir.path());
    (dir, file)
}

fn point_read_operation(file: &Path) -> Json {
    let program = with_loaded_project(file, None, |workspace, _| {
        workspace.program().expect("checked program").clone()
    })
    .expect("surface project checks");
    let abi = marrow_json::surface::SurfaceAbiJson::from_program(&program);
    let manifest = marrow_json::surface::SurfaceRouteManifestJson::from_abi(&abi);
    let route = manifest
        .routes
        .iter()
        .find(|route| route.alias == "get")
        .expect("point read route");
    let read = abi
        .surfaces
        .iter()
        .flat_map(|surface| surface.read.iter())
        .find(|read| read.operation_tag == route.operation_tag)
        .expect("point read descriptor");

    json!({
        "profile_version": "surface.operation.v1",
        "operation_tag": route.operation_tag,
        "request": {
            "kind": "point_read",
            "request": {
                "identity": {
                    "store_catalog_id": read.store_catalog_id,
                    "keys": [{ "kind": "int", "value": "1" }]
                }
            }
        }
    })
}

fn update_author_operation(file: &Path, value: &str) -> Json {
    let program = with_loaded_project(file, None, |workspace, _| {
        workspace.program().expect("checked program").clone()
    })
    .expect("surface project checks");
    let abi = marrow_json::surface::SurfaceAbiJson::from_program(&program);
    let update = abi
        .surfaces
        .iter()
        .find_map(|surface| surface.update.as_ref())
        .expect("update descriptor");
    let author = update
        .fields
        .iter()
        .find(|field| field.render_label == "author")
        .expect("author update field");

    json!({
        "profile_version": "surface.operation.v1",
        "operation_tag": update.operation_tag,
        "request": {
            "kind": "point_update",
            "request": {
                "identity": {
                    "store_catalog_id": update.store_catalog_id,
                    "keys": [{ "kind": "int", "value": "1" }]
                },
                "fields": [{
                    "catalog_id": author.member_catalog_id,
                    "value": { "kind": "string", "value": value }
                }]
            }
        }
    })
}

fn retitle_action_operation(file: &Path, title: &str) -> Json {
    let program = with_loaded_project(file, None, |workspace, _| {
        workspace.program().expect("checked program").clone()
    })
    .expect("surface project checks");
    let abi = marrow_json::surface::SurfaceAbiJson::from_program(&program);
    let action = abi
        .surfaces
        .iter()
        .flat_map(|surface| surface.actions.iter())
        .find(|action| action.alias == "retitle")
        .expect("retitle action descriptor");

    json!({
        "profile_version": "surface.operation.v1",
        "operation_tag": action.operation_tag,
        "request": {
            "kind": "action",
            "request": {
                "arguments": [
                    {
                        "name": "id",
                        "value": { "kind": "int", "value": "1" }
                    },
                    {
                        "name": "title",
                        "value": { "kind": "string", "value": title }
                    }
                ]
            }
        }
    })
}

fn read_field(result: &Json, name: &str) -> Json {
    result["response"]["result"]["record"]["fields"]
        .as_array()
        .expect("record fields")
        .iter()
        .find(|field| field["render_label"] == name)
        .unwrap_or_else(|| panic!("field {name} in {result}"))["value"]
        .clone()
}

#[test]
fn check_reports_a_return_type_error_for_a_project_overlay() {
    let (_dir, file) = project();
    let overlay = "module shelf::books\n\npub fn answer(): int\n    return \"nope\"\n";
    let result = check(Some(&file), Some(overlay));
    let diagnostics = result["diagnostics"].as_array().unwrap();
    assert!(
        diagnostics.iter().any(|d| d["code"] == "check.return_type"),
        "expected a check.return_type diagnostic, got {result}"
    );
}

#[test]
fn check_of_a_clean_file_has_no_diagnostics() {
    let (_dir, file) = project();
    let result = check(Some(&file), None);
    assert_eq!(
        result["diagnostics"].as_array().unwrap().len(),
        0,
        "{result}"
    );
}

/// A native-store project stamped with a committed catalog baseline and no
/// `marrow.lock`, so the store is the sole accepted authority. Returns the source file.
fn stamped_store_no_lock_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" } }"#,
    )
    .unwrap();
    let src = root.join("src/app");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("counter.mw"),
        "module app::counter\n\nresource Counter\n    required value: int\n\n\
         store ^counter(id: int): Counter\n",
    )
    .unwrap();

    let config = parse_config(&std::fs::read_to_string(root.join("marrow.json")).unwrap()).unwrap();
    let program = check_project_against(root, &config, None, None).unwrap();
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let store = SealedStore::open(&data_dir.join("marrow.redb"), AccessMode::Create)
        .unwrap()
        .into_store();
    store
        .write_store_uid(&StoreUid::new("store_00000000000000000000000000000001").unwrap())
        .unwrap();
    marrow_run::evolution::commit_catalog_baseline(&store, &program).unwrap();
    drop(store);
    assert!(
        !root.join(CATALOG_FILE_NAME).exists(),
        "the fixture must have no committed lock, so only the store carries accepted identity"
    );

    (dir, src.join("counter.mw"))
}

/// `mw_check` must check against the store's committed accepted identity, not a fresh
/// first run. Adding a required field to a resource whose identity the store already
/// committed is an unrecorded-identity change the checker flags only when the accepted
/// catalog is bound. The storeless bug analyzed every project as a first run, which
/// baselines the new shape silently and surfaces nothing.
#[test]
fn check_reflects_the_committed_accepted_identity_of_a_stamped_store() {
    let (_dir, file) = stamped_store_no_lock_project();
    let evolved = "module app::counter\n\nresource Counter\n    required value: int\n    \
                   required label: string\n\nstore ^counter(id: int): Counter\n";
    let result = check(Some(&file), Some(evolved));
    let diagnostics = result["diagnostics"].as_array().unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|d| d["code"] == "check.catalog_intent"),
        "a stamped store's committed identity must drive the check, surfacing the added \
         required field as an unrecorded-identity change, got {result}"
    );
}

#[test]
fn check_snippet_reports_a_syntax_error() {
    // A bare snippet with a broken declaration: no project, parser diagnostics.
    let result = check(None, Some("module a\n\npub fn broken(:\n"));
    assert!(
        !result["diagnostics"].as_array().unwrap().is_empty(),
        "a malformed snippet should report at least one diagnostic, got {result}"
    );
}

#[test]
fn type_at_reports_the_type_of_a_parameter_use() {
    let (_dir, file) = project();
    let position = position_in_file(&file, "n * 2");
    let result = type_at_position(&file, position.line, position.character);
    assert_eq!(
        result["type"], "int",
        "the `n` parameter use is an int: {result}"
    );
}

#[test]
fn complete_in_a_function_body_lists_locals_and_keywords() {
    let (_dir, file) = project();
    let position = position_in_file(&file, "n * 2");
    let result = complete(&file, position.line, position.character);
    let items = result["items"].as_array().unwrap();
    assert!(
        !items.is_empty(),
        "completion should offer items, got {result}"
    );
    assert!(
        items
            .iter()
            .any(|item| item["label"] == "n" && item["kind"] == "variable"),
        "the `n` parameter should be an in-scope completion, got {result}"
    );
    assert!(
        items.iter().any(|item| item["kind"] == "keyword"),
        "a bare position should offer keywords, got {result}"
    );
    assert!(
        items.iter().any(|item| {
            item["label"] == "key"
                && item["kind"] == "function"
                && item["detail"] == "key(id): value"
        }),
        "canonical key builtin should be offered, got {result}"
    );
    assert!(
        items.iter().all(|item| item["label"] != "write"),
        "removed builtins must not be offered, got {result}"
    );
    assert_completion_contract(&result);
}

#[test]
fn complete_lists_import_aliases_as_module_candidates() {
    let (_dir, file) = namespace_project();
    let position = position_in_file(&file, "return 1");
    let result = complete(&file, position.line, position.character);
    let items = result["items"].as_array().unwrap();
    assert!(
        items.iter().any(|item| {
            item["label"] == "books"
                && item["kind"] == "module"
                && item["detail"] == "module shelf::books"
        }),
        "import aliases must be module completions from Marrow facts, got {result}"
    );
    assert!(
        items.iter().any(|item| {
            item["label"] == "std"
                && item["kind"] == "module"
                && item["detail"] == "standard library"
        }),
        "raw std namespace must be a Marrow module completion, got {result}"
    );
    assert!(
        items.iter().any(|item| {
            item["label"] == "run"
                && item["kind"] == "function"
                && item["detail"] == "fn run(): int"
        }),
        "same-module functions must be Marrow completion facts, got {result}"
    );
}

#[test]
fn complete_namespace_items_include_docs_in_mcp_json() {
    let (_dir, file) = namespace_project();
    std::fs::write(
        &file,
        "\
module shelf::app

use shelf::books

pub fn run(): int
    const status = books::chooseStatus(\"current\", books::Status::active)
    return 1
",
    )
    .unwrap();
    let position = position_in_file(&file, "chooseStatus");
    let result = complete(&file, position.line, position.character);
    let items = result["items"].as_array().unwrap();
    assert!(
        items.iter().any(|item| {
            item["label"] == "Book"
                && item["kind"] == "resource"
                && item["docs"] == json!(["Books stored in the public shelf."])
        }),
        "MCP completion must preserve Marrow item docs, got {result}"
    );
    assert_completion_contract(&result);
}

#[test]
fn complete_expected_enum_values_uses_marrow_completion_fact_items() {
    let (_dir, file) = namespace_project();
    for source in [
        "\
module shelf::app

use shelf::books

pub fn run()
    const state: Status = candidate
",
        "\
module shelf::app

use shelf::books

pub fn run()
    const state = books::chooseStatus(\"current\", candidate)
",
        "\
module shelf::app

use shelf::books

resource Draft
    required title: string
    required state: books::Status

pub fn run()
    const draft = Draft(title: \"draft\", state: candidate)
",
        "\
module shelf::app

use shelf::books

pub fn run()
    var state: Status = Status::active
    state = candidate
",
    ] {
        std::fs::write(&file, source).unwrap();
        let position = position_in_file(&file, "candidate");
        let result = complete(&file, position.line, position.character);
        let items = result["items"].as_array().unwrap();
        assert!(
            items.iter().any(|item| {
                item["label"] == "Status::active"
                    && item["kind"] == "enum-member"
                    && item["detail"] == "Status"
            }),
            "expected enum value completion should include the selectable member, got {result}"
        );
        assert!(
            items.iter().any(|item| {
                item["label"] == "Status::archived::retired"
                    && item["kind"] == "enum-member"
                    && item["detail"] == "Status"
            }),
            "expected enum value completion should include nested selectable leaves, got {result}"
        );
        assert!(
            items.iter().all(|item| item["label"] != "Status::archived"),
            "expected enum value completion must not offer the category member, got {result}"
        );
        assert_completion_contract(&result);
    }

    std::fs::write(
        &file,
        "\
module shelf::app

use shelf::books

pub fn run()
    var state: Status = Status::active
    state =
",
    )
    .unwrap();
    let position = position_after_in_file(&file, "    state =");
    let result = complete(&file, position.line, position.character);
    let items = result["items"].as_array().unwrap();
    assert!(
        items.iter().any(|item| {
            item["label"] == "Status::active"
                && item["kind"] == "enum-member"
                && item["detail"] == "Status"
        }),
        "empty assignment completion should include enum values, got {result}"
    );
    assert!(
        items.iter().all(|item| item["label"] != "Status::archived"),
        "empty assignment completion must not offer category members, got {result}"
    );

    std::fs::write(
        &file,
        "\
module shelf::app

use shelf::books

resource Local
    required state: books::Status

pub fn run()
    const draft = Local(state: )
",
    )
    .unwrap();
    let position = position_after_in_file(&file, "Local(state: ");
    let result = complete(&file, position.line, position.character);
    let items = result["items"].as_array().unwrap();
    assert!(
        items.iter().any(|item| {
            item["label"] == "Status::active"
                && item["kind"] == "enum-member"
                && item["detail"] == "Status"
        }),
        "empty resource constructor enum field should include enum values, got {result}"
    );
    assert!(
        items.iter().all(|item| item["label"] != "Status::archived"),
        "empty resource constructor enum field must not offer category members, got {result}"
    );

    std::fs::write(
        &file,
        "\
module shelf::app

use shelf::books

resource Local
    required state: books::Status

resource Local
    required title: string

pub fn run()
    const draft = Local(state: )
",
    )
    .unwrap();
    let position = position_after_in_file(&file, "Local(state: ");
    let result = complete(&file, position.line, position.character);
    let items = result["items"].as_array().unwrap();
    assert!(
        items.iter().all(|item| item["label"] != "Status::active"),
        "duplicate current-source resources must fail closed instead of offering enum completions, got {result}"
    );

    std::fs::write(
        &file,
        "\
module shelf::app

use shelf::books

resource Local
    required state: books::Status

fn Local(value: string): string
    return value

pub fn run()
    const draft = Local(state: )
",
    )
    .unwrap();
    let position = position_after_in_file(&file, "Local(state: ");
    let result = complete(&file, position.line, position.character);
    let items = result["items"].as_array().unwrap();
    assert!(
        items.iter().all(|item| item["label"] != "Status::active"),
        "current-source resource names colliding with another top-level declaration must fail closed, got {result}"
    );

    for (label, source, marker) in [
        (
            "annotated const initializer empty string argument",
            "\
module shelf::app

use shelf::books

pub fn run()
    const state: Status = books::chooseStatus(, Status::active)
",
            "chooseStatus(",
        ),
        (
            "assignment empty string argument",
            "\
module shelf::app

use shelf::books

pub fn run()
    var state: Status = Status::active
    state = books::chooseStatus(, Status::active)
",
            "chooseStatus(",
        ),
        (
            "return empty string argument",
            "\
module shelf::app

use shelf::books

pub fn run(): Status
    return books::chooseStatus(, Status::active)
",
            "chooseStatus(",
        ),
    ] {
        std::fs::write(&file, source).unwrap();
        let position = position_after_in_file(&file, marker);
        let result = complete(&file, position.line, position.character);
        let items = result["items"].as_array().unwrap();
        assert!(
            items.iter().all(|item| item["label"] != "Status::active"),
            "{label}: outer expected enum values must not leak into empty string call arguments, got {result}"
        );
    }

    std::fs::write(
        &file,
        "\
module shelf::app

use shelf::books

pub fn run(state: Status)
    match state
        candidate
            return
",
    )
    .unwrap();
    let position = position_in_file(&file, "candidate");
    let result = complete(&file, position.line, position.character);
    let items = result["items"].as_array().unwrap();
    assert!(
        items.iter().any(|item| {
            item["label"] == "active"
                && item["kind"] == "enum-member"
                && item["detail"] == "Status match arm"
        }),
        "match arm completion should include relative selectable members, got {result}"
    );
    assert!(
        items.iter().any(|item| {
            item["label"] == "archived"
                && item["kind"] == "enum-member"
                && item["detail"] == "Status match arm"
        }),
        "match arm completion should include category arms, got {result}"
    );
    assert!(
        items.iter().any(|item| {
            item["label"] == "archived::retired"
                && item["kind"] == "enum-member"
                && item["detail"] == "Status match arm"
        }),
        "match arm completion should include nested relative members, got {result}"
    );
    assert!(
        items.iter().all(|item| item["label"] != "Status::active"),
        "match arm completions must stay relative to the scrutinee enum, got {result}"
    );
    assert_completion_contract(&result);

    let empty_match_arm_source = concat!(
        "\
module shelf::app

use shelf::books

pub fn run(state: Status)
    match state
",
        "        ",
        "\n",
    );
    std::fs::write(&file, empty_match_arm_source).unwrap();
    let position = position_after_in_file(&file, "    match state\n        ");
    let result = complete(&file, position.line, position.character);
    let items = result["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|item| item["label"] == "active" && item["kind"] == "enum-member"),
        "empty match arm completion should include relative selectable members, got {result}"
    );
    assert!(
        items.iter().all(|item| item["label"] != "Status::active"),
        "empty match arm completions must stay relative, got {result}"
    );
}

#[test]
fn complete_saved_path_members_uses_marrow_active_saved_path_fact() {
    let (_dir, file) = project();
    std::fs::write(
        &file,
        "\
module shelf::books

resource Book
    required title: string
    code: ErrorCode
    tags(pos: int): string

store ^books(id: int): Book

pub fn read(id: int)
    const value = ^books(id).candidate
",
    )
    .unwrap();
    let position = position_in_file(&file, "candidate");
    let result = complete(&file, position.line, position.character);
    let items = result["items"].as_array().unwrap();
    assert!(
        items.iter().any(|item| {
            item["label"] == "title"
                && item["kind"] == "field"
                && item["detail"] == "required field: string"
        }),
        "saved-path completion should include required fields from Marrow facts, got {result}"
    );
    assert_completion_contract(&result);
}

#[test]
fn namespace_complete_for_imported_module_returns_marrow_fact_shape() {
    let (_dir, file) = namespace_project();
    let result = namespace_complete(&file, &["books".to_string()]);
    assert_eq!(result["profile_version"], "source.namespace.completion.v1");
    assert_eq!(result["kind"], "module");
    assert_production_contract(&result, "source namespace completion fact");
    assert_eq!(
        result["contract"]["basis"],
        "Marrow source namespace completion facts"
    );
    assert_eq!(result["module"], "shelf::books");

    let resources = result["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1, "{result}");
    assert_eq!(resources[0]["name"], "Book");
    assert_eq!(
        resources[0]["docs"],
        json!(["Books stored in the public shelf."])
    );

    let enums = result["enums"].as_array().unwrap();
    assert_eq!(enums.len(), 1, "{result}");
    assert_eq!(enums[0]["name"], "Status");
    assert_eq!(enums[0]["docs"], json!(["Public reading status."]));

    let functions = result["functions"].as_array().unwrap();
    let names: Vec<&str> = functions
        .iter()
        .map(|function| function["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["titleOf", "chooseStatus", "shout"]);
    let title = functions
        .iter()
        .find(|function| function["name"] == "titleOf")
        .unwrap();
    assert_eq!(title["docs"], json!([]));
    assert_eq!(title["return_type"], "string");
    assert_eq!(title["params"][0]["name"], "id");
    assert_eq!(title["params"][0]["type"], "Id(^books)");
    assert_eq!(title["params"][0]["docs"], json!([]));
    assert_eq!(title["params"][1]["name"], "fallback");
    assert_eq!(title["params"][1]["type"], "string");
    assert_eq!(title["params"][1]["docs"], json!([]));
}

#[test]
fn namespace_complete_for_enum_returns_member_statuses_and_docs() {
    let (_dir, file) = namespace_project();
    let result = namespace_complete(&file, &["books".to_string(), "Status".to_string()]);
    assert_eq!(result["profile_version"], "source.namespace.completion.v1");
    assert_eq!(result["kind"], "enum");
    assert_production_contract(&result, "source namespace completion fact");
    assert_eq!(result["enum_name"], "Status");

    let members = result["members"].as_array().unwrap();
    let member = |name: &str| {
        members
            .iter()
            .find(|member| member["name"] == name)
            .unwrap_or_else(|| panic!("missing enum member {name}: {result}"))
    };
    assert_eq!(member("active")["status"], "selectable");
    assert_eq!(member("active")["docs"], json!(["Ready for use."]));
    assert_eq!(member("archived")["status"], "category");
    assert_eq!(member("archived")["docs"], json!(["Historical status."]));
    assert_eq!(member("retired")["status"], "selectable");
    assert_eq!(member("retired")["docs"], json!(["Out of print."]));
}

#[test]
fn namespace_complete_for_unknown_qualifier_returns_empty_ready_result() {
    let (_dir, file) = namespace_project();
    let result = namespace_complete(&file, &["books".to_string(), "Missing".to_string()]);
    assert_eq!(result["profile_version"], "source.namespace.completion.v1");
    assert_eq!(result["kind"], Json::Null);
    assert_eq!(result["resources"], json!([]));
    assert_eq!(result["enums"], json!([]));
    assert_eq!(result["functions"], json!([]));
    assert_eq!(result["members"], json!([]));
    assert_production_contract(&result, "source namespace completion fact");
}

#[test]
fn namespace_complete_for_import_alias_shadowed_by_local_binding_returns_empty_ready_result() {
    let (_dir, file) = namespace_project_with_local_alias_collision();
    let result = namespace_complete(&file, &["books".to_string()]);
    assert_eq!(result["profile_version"], "source.namespace.completion.v1");
    assert_eq!(result["kind"], Json::Null);
    assert_eq!(result["resources"], json!([]));
    assert_eq!(result["enums"], json!([]));
    assert_eq!(result["functions"], json!([]));
    assert_eq!(result["members"], json!([]));
    assert_production_contract(&result, "source namespace completion fact");

    let result = namespace_complete(&file, &["books".to_string(), "Status".to_string()]);
    assert_eq!(result["profile_version"], "source.namespace.completion.v1");
    assert_eq!(result["kind"], Json::Null);
    assert_eq!(result["resources"], json!([]));
    assert_eq!(result["enums"], json!([]));
    assert_eq!(result["functions"], json!([]));
    assert_eq!(result["members"], json!([]));
    assert_production_contract(&result, "source namespace completion fact");

    let result = namespace_complete(
        &file,
        &[
            "shelf".to_string(),
            "books".to_string(),
            "Status".to_string(),
        ],
    );
    assert_eq!(result["profile_version"], "source.namespace.completion.v1");
    assert_eq!(result["kind"], "enum");
    assert_eq!(result["enum_name"], "Status");
    assert_production_contract(&result, "source namespace completion fact");
}

#[test]
fn namespace_complete_for_import_alias_shadowed_by_evolve_body_returns_empty_ready_result() {
    let (_dir, file) = namespace_project_with_evolve_alias_collision();
    let result = namespace_complete(&file, &["books".to_string()]);
    assert_eq!(result["profile_version"], "source.namespace.completion.v1");
    assert_eq!(result["kind"], Json::Null);
    assert_eq!(result["resources"], json!([]));
    assert_eq!(result["enums"], json!([]));
    assert_eq!(result["functions"], json!([]));
    assert_eq!(result["members"], json!([]));
    assert_production_contract(&result, "source namespace completion fact");

    let result = namespace_complete(&file, &["books".to_string(), "Status".to_string()]);
    assert_eq!(result["profile_version"], "source.namespace.completion.v1");
    assert_eq!(result["kind"], Json::Null);
    assert_eq!(result["resources"], json!([]));
    assert_eq!(result["enums"], json!([]));
    assert_eq!(result["functions"], json!([]));
    assert_eq!(result["members"], json!([]));
    assert_production_contract(&result, "source namespace completion fact");

    let result = namespace_complete(
        &file,
        &[
            "shelf".to_string(),
            "books".to_string(),
            "Status".to_string(),
        ],
    );
    assert_eq!(result["profile_version"], "source.namespace.completion.v1");
    assert_eq!(result["kind"], "enum");
    assert_eq!(result["enum_name"], "Status");
    assert_production_contract(&result, "source namespace completion fact");
}

#[test]
fn resource_schema_shapes_the_book_resource() {
    let (_dir, file) = project();
    let result = resource_schema(&file, "Book");
    assert_eq!(
        result["profile_version"], "resource.schema.v1",
        "Marrow resource schema profile must be present: {result}"
    );
    assert_eq!(result["diagnostics"], json!([]), "{result}");
    let resources = result["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1, "one Book resource: {result}");
    let book = &resources[0];
    assert_eq!(book["module"], "shelf::books");
    assert_eq!(book["name"], "Book");
    assert!(book["catalogId"].is_string(), "{result}");
    let stores = book["stores"].as_array().unwrap();
    assert_eq!(stores.len(), 1);
    let store = &stores[0];
    assert_eq!(store["root"], "books");
    assert_eq!(store["resource"], "Book");
    assert!(store["catalogId"].is_string(), "{result}");
    assert_eq!(store["identityKeys"][0]["name"], "id");
    assert_eq!(store["identityKeys"][0]["type"], "int");
    let members = book["members"].as_array().unwrap();
    assert!(members.iter().any(|m| m["name"] == "title"
        && m["catalogId"].is_string()
        && m["kind"] == "field"
        && m["type"] == "string"));
    assert!(members.iter().any(|m| {
        m["name"] == "code"
            && m["catalogId"].is_string()
            && m["kind"] == "field"
            && m["type"] == "ErrorCode"
            && m["errorCode"] == true
    }));
    assert!(
        members
            .iter()
            .any(|m| m["name"] == "tags" && m["kind"] == "leaf")
    );
    assert!(
        store["indexes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["name"] == "byTitle" && i["catalogId"].is_string())
    );
    assert_production_contract(&result, "resource schema DTO");
    assert_eq!(result["contract"]["basis"], "Marrow resource schema DTO");
}

#[test]
fn resource_schema_refuses_ambiguous_resource_names() {
    let (dir, file) = project();
    let other = dir.path().join("src/other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("books.mw"),
        "\
module other::books

resource Book
    required code: string
",
    )
    .unwrap();

    let result = resource_schema(&file, "Book");
    assert_eq!(result["resources"], json!([]), "{result}");
    assert_eq!(
        result["diagnostics"][0]["code"], "resource.schema.identity",
        "{result}"
    );
    assert_production_contract(&result, "resource schema DTO");
    assert_eq!(result["contract"]["basis"], "Marrow resource schema DTO");
}

#[test]
fn surface_routes_returns_read_only_manifest_without_data_gate() {
    let (_dir, file) = catalog_only_surface_project();

    let result = surface_routes(&file, SurfaceRouteScope::ReadOnly);

    assert_eq!(result["profile_version"], "surface.route.v1", "{result}");
    assert_eq!(
        result["operation_profile_version"], "surface.operation.v1",
        "{result}"
    );
    let routes = result["routes"].as_array().expect("surface routes");
    let aliases = routes
        .iter()
        .map(|route| route["alias"].as_str().expect("route alias"))
        .collect::<Vec<_>>();
    assert_eq!(aliases, vec!["get", "byAuthor"], "{result}");
    assert!(
        routes.iter().all(
            |route| route["request"]["kind"].as_str().is_some_and(|kind| {
                matches!(
                    kind,
                    "point_read" | "page" | "singleton_read" | "unique_lookup"
                )
            })
        ),
        "mw_surface_routes must expose read routes only: {result}"
    );
    assert!(
        routes.iter().all(|route| route["path"]
            .as_str()
            .is_some_and(|path| path.starts_with("/surface/v1/read/"))),
        "mw_surface_routes must not expose update/action paths: {result}"
    );
    assert_production_contract(&result, "surface read route manifest");
}

#[test]
fn surface_routes_all_scope_includes_write_and_action_routes_without_data_gate() {
    let (_dir, file) = catalog_only_surface_project();

    let result = surface_routes(&file, SurfaceRouteScope::All);

    let routes = result["routes"].as_array().expect("surface routes");
    let aliases = routes
        .iter()
        .map(|route| route["alias"].as_str().expect("route alias"))
        .collect::<Vec<_>>();
    assert_eq!(aliases, vec!["get", "byAuthor", "update", "retitle"]);
    assert!(
        routes.iter().any(|route| route["path"]
            .as_str()
            .is_some_and(|path| path.starts_with("/surface/v1/update/"))),
        "all-scope routes must expose update paths: {result}"
    );
    assert!(
        routes.iter().any(|route| route["path"]
            .as_str()
            .is_some_and(|path| path.starts_with("/surface/v1/action/"))),
        "all-scope routes must expose action paths: {result}"
    );
    assert_production_contract(&result, "surface route manifest");
}

#[test]
fn surface_read_refuses_before_project_loading_when_data_access_is_disabled() {
    let request = json!({
        "profile_version": "surface.operation.v1",
        "operation_tag": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "request": { "kind": "singleton_read" }
    });

    let result = surface_read(Path::new("/nope/project/src/main.mw"), request, None)
        .expect("valid operation request");

    assert_eq!(result["available"], false, "{result}");
    assert_eq!(result["dataAccess"], "disabled", "{result}");
    assert!(
        result.get("error").is_none(),
        "disabled surface reads must not reach project loading: {result}"
    );
    assert_production_contract(&result, "surface read operation");
}

#[test]
fn surface_write_refuses_before_project_loading_when_data_access_is_disabled() {
    let request = json!({
        "profile_version": "surface.operation.v1",
        "operation_tag": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "request": {
            "kind": "point_update",
            "request": {
                "identity": {
                    "store_catalog_id": "cat_00000000000000000000000000000000",
                    "keys": [{ "kind": "int", "value": "1" }]
                },
                "fields": []
            }
        }
    });

    let result = surface_write(Path::new("/nope/project/src/main.mw"), request, None)
        .expect("valid operation request");

    assert_eq!(result["available"], false, "{result}");
    assert_eq!(result["dataAccess"], "disabled", "{result}");
    assert!(
        result.get("error").is_none(),
        "disabled surface writes must not reach project loading: {result}"
    );
    assert_production_contract(&result, "surface write operation");
    assert_eq!(
        result["contract"]["basis"],
        "Marrow surface operation writable executor"
    );
    assert_eq!(result["contract"]["operations"], "read/update/action");
}

#[test]
fn surface_write_executes_canonical_point_update_request() {
    let (_dir, file) = native_surface_project();
    let before = surface_read(&file, point_read_operation(&file), granted_read())
        .expect("valid read operation");

    let result = surface_write(
        &file,
        update_author_operation(&file, "Brian Herbert"),
        granted_write(),
    )
    .expect("valid update operation");
    let after = surface_read(&file, point_read_operation(&file), granted_read())
        .expect("valid read operation");

    assert_eq!(result["available"], true, "{result}");
    assert_eq!(result["response"]["result"]["kind"], "updated", "{result}");
    assert!(
        result["store_stamp"]["commit_id"].as_u64() > before["store_stamp"]["commit_id"].as_u64(),
        "surface write must advance the store commit id: before={before} write={result}"
    );
    assert_eq!(
        read_field(&after, "author"),
        json!({ "kind": "string", "value": "Brian Herbert" }),
        "{after}"
    );
    assert!(
        after["store_stamp"]["commit_id"].as_u64() >= result["store_stamp"]["commit_id"].as_u64(),
        "later reads must observe the write stamp: write={result} after={after}"
    );
    assert_production_contract(&result, "surface write operation");
}

#[test]
fn every_surface_write_appends_an_audit_record() {
    let (dir, file) = native_surface_project();
    let audit_path = dir.path().join("marrow-mcp-write-audit.jsonl");
    assert!(
        !audit_path.exists(),
        "no audit trail should exist before any write"
    );

    let result = surface_write(
        &file,
        update_author_operation(&file, "Frank Herbert"),
        granted_write(),
    )
    .expect("valid update operation");

    // The write envelope carries the audit record the trail persisted, so an agent
    // sees exactly what was recorded about its own mutation.
    let audit = &result["audit"];
    assert_eq!(audit["request_kind"], "point_update", "{result}");
    assert!(
        audit["operation_tag"]
            .as_str()
            .is_some_and(|tag| !tag.is_empty()),
        "the audit record names the operation tag: {result}"
    );
    assert_eq!(audit["file"], file.to_str().unwrap(), "{result}");
    assert!(
        audit["timestamp_ms"].as_u64().is_some_and(|ms| ms > 0),
        "the audit record stamps the write with an epoch-millis timestamp: {result}"
    );
    assert_eq!(
        audit["outcome"], "ok",
        "a committed write audits ok: {result}"
    );
    // The target is the operation's identity — its store and keys — never the
    // written field values, so the trail names the record touched without copying
    // stored data into itself.
    assert!(
        audit["target"]["store_catalog_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("cat_")),
        "the audit target carries the operation's store identity: {result}"
    );
    assert_eq!(
        audit["target"]["keys"],
        json!([{ "kind": "int", "value": "1" }]),
        "the audit target carries the operation's keys, not the written value: {result}"
    );
    assert!(
        audit
            .get("target")
            .and_then(|target| target.get("fields"))
            .is_none(),
        "the audit target must not carry the written field values: {result}"
    );
    assert!(
        audit["store_after"]["commit_id"].as_u64() > audit["store_before"]["commit_id"].as_u64(),
        "the audit record must show the write advanced the store: {result}"
    );
    assert!(result.get("audit_error").is_none(), "{result}");

    // The trail is a durable, append-only JSONL file beside the project: one line
    // per write, so it grows rather than being overwritten.
    let first = std::fs::read_to_string(&audit_path).expect("audit trail was written");
    assert_eq!(
        first.lines().count(),
        1,
        "one record after one write: {first}"
    );
    let recorded: Json = serde_json::from_str(first.lines().next().unwrap()).unwrap();
    assert_eq!(
        recorded, *audit,
        "the persisted record matches the envelope"
    );

    surface_write(
        &file,
        update_author_operation(&file, "Brian Herbert"),
        granted_write(),
    )
    .expect("second write");
    let both = std::fs::read_to_string(&audit_path).expect("audit trail still present");
    assert_eq!(
        both.lines().count(),
        2,
        "a second write appends rather than replacing: {both}"
    );
}

#[test]
fn surface_write_executes_canonical_action_request_and_returns_canonical_errors() {
    let (_dir, file) = native_surface_project();

    let result = surface_write(
        &file,
        retitle_action_operation(&file, "Dune MCP"),
        granted_write(),
    )
    .expect("valid action operation");
    let after = surface_read(&file, point_read_operation(&file), granted_read())
        .expect("valid read operation");

    assert_eq!(result["available"], true, "{result}");
    assert_eq!(result["response"]["result"]["kind"], "action", "{result}");
    assert_eq!(
        result["response"]["result"]["result"]["value"],
        json!({ "kind": "string", "value": "Dune MCP" }),
        "{result}"
    );
    assert_eq!(
        read_field(&after, "title"),
        json!({ "kind": "string", "value": "Dune MCP" }),
        "{after}"
    );

    let action_request = retitle_action_operation(&file, "unused");
    let mut wrong_body = update_author_operation(&file, "Wrong Body");
    wrong_body["operation_tag"] = action_request["operation_tag"].clone();
    let mismatch = surface_write(&file, wrong_body, granted_write()).expect("valid operation DTO");
    assert_eq!(mismatch["available"], true, "{mismatch}");
    assert_eq!(mismatch["error"]["code"], "surface.request", "{mismatch}");
    assert_eq!(
        mismatch["error"]["message"],
        "surface operation request body does not match the operation tag",
        "{mismatch}"
    );
    assert_production_contract(&mismatch, "surface write operation");
}

#[test]
fn surface_read_executes_canonical_point_read_request() {
    let (_dir, file) = native_surface_project();

    let result = surface_read(&file, point_read_operation(&file), granted_read())
        .expect("valid operation request");

    assert_eq!(result["available"], true, "{result}");
    assert_eq!(
        result["response"]["profile_version"], "surface.operation.v1",
        "{result}"
    );
    assert_eq!(result["response"]["result"]["kind"], "record", "{result}");
    assert!(
        result["store_stamp"]["store_uid"]
            .as_str()
            .is_some_and(|uid| uid.starts_with("store_")),
        "surface reads must carry the Marrow store stamp: {result}"
    );
    assert!(
        result["store_stamp"]["catalog_epoch"].is_u64(),
        "surface reads must carry the Marrow catalog epoch: {result}"
    );
    assert!(
        result["store_stamp"]["commit_id"].is_u64(),
        "surface reads must carry the Marrow commit id: {result}"
    );
    let record = &result["response"]["result"]["record"];
    assert_eq!(
        record["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .find(|field| field["render_label"] == "title")
            .and_then(|field| field["value"].as_object().map(|_| field["value"].clone())),
        Some(json!({ "kind": "string", "value": "Dune" })),
        "{result}"
    );
    assert_production_contract(&result, "surface read operation");
}

#[test]
fn surface_read_returns_canonical_error_for_write_bodies() {
    let (_dir, file) = native_surface_project();
    let program = with_loaded_project(&file, None, |workspace, _| {
        workspace.program().expect("checked program").clone()
    })
    .expect("surface project checks");
    let abi = marrow_json::surface::SurfaceAbiJson::from_program(&program);
    let update_tag = abi
        .surfaces
        .iter()
        .find_map(|surface| surface.update.as_ref())
        .map(|update| update.operation_tag.clone())
        .expect("update operation tag");
    let request = json!({
        "profile_version": "surface.operation.v1",
        "operation_tag": update_tag,
        "request": {
            "kind": "point_update",
            "request": {
                "identity": {
                    "store_catalog_id": "cat_00000000000000000000000000000000",
                    "keys": [{ "kind": "int", "value": "1" }]
                },
                "fields": []
            }
        }
    });

    let result =
        surface_read(&file, request, granted_read()).expect("write operation request decodes");

    assert_eq!(result["available"], true, "{result}");
    assert!(
        result["store_stamp"]["store_uid"]
            .as_str()
            .is_some_and(|uid| uid.starts_with("store_")),
        "canonical surface errors must carry the read-session store stamp: {result}"
    );
    assert_eq!(result["error"]["code"], "surface.abi_mismatch", "{result}");
    assert_eq!(
        result["error"]["message"],
        "surface operation request requires a writable project surface session",
        "{result}"
    );
    assert_production_contract(&result, "surface read operation");
}

#[test]
fn run_executes_a_pure_function_and_returns_its_value() {
    let (_dir, file) = pure_project();
    let result = run(
        &file,
        Some("shelf::books::shout"),
        &[],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );
    assert_eq!(result["diagnostics"].as_array().unwrap().len(), 0);
    assert!(
        result["output"].as_str().unwrap().contains("loud"),
        "zero-argument run should execute: {result}"
    );
    assert_run_contract(&result);
}

/// A pure project whose entries loop: `spin` never returns, and `chatter` prints
/// far more than the output cap before returning. Both drive the run watchdog.
fn looping_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" }, "tests": ["tests"] }"#,
    )
    .unwrap();
    let src = root.join("src/shelf");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("loops.mw"),
        "\
module shelf::loops

pub fn spin()
    var i = 0
    while true
        i = i + 1

pub fn chatter()
    var i = 0
    while i < 5000
        print(\"marrow captured output padding line for the cap test\")
        i = i + 1
",
    )
    .unwrap();
    let file = src.join("loops.mw");
    (dir, file)
}

#[test]
fn run_reports_budget_exceeded_for_a_looping_entry_within_the_budget() {
    let (_dir, file) = looping_project();
    let budget = Duration::from_millis(300);
    let started = std::time::Instant::now();
    let result = run(&file, Some("shelf::loops::spin"), &[], RunMode::Run, budget);
    let elapsed = started.elapsed();
    assert_eq!(
        result["budget_exceeded"], true,
        "a looping run must report the budget fault: {result}"
    );
    assert_eq!(
        result["diagnostics"][0]["code"], "mcp.run.budget_exceeded",
        "the fault carries a stable code: {result}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the watchdog must return near the budget, not run forever: {elapsed:?}"
    );
    assert_run_contract(&result);
}

#[test]
fn run_caps_captured_output_and_flags_truncation() {
    let (_dir, file) = looping_project();
    let result = run(
        &file,
        Some("shelf::loops::chatter"),
        &[],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );
    assert_eq!(
        result["output_truncated"], true,
        "a print-heavy run must flag truncated output: {result}"
    );
    let output = result["output"].as_str().unwrap();
    assert!(
        output.len() <= 64 * 1024,
        "captured output must stay under the cap, got {} bytes",
        output.len()
    );
    assert!(
        output.contains("marrow captured output"),
        "the retained prefix must hold real output: {result}"
    );
    assert_run_contract(&result);
}

#[test]
fn tool_calls_reuse_the_cached_workspace_until_source_changes() {
    reset_project_cache();
    let (_dir, file) = namespace_project();
    let books = file.with_file_name("books.mw");

    // The first tool call on a cold cache checks the project.
    let _ = check(Some(&file), None);
    assert_eq!(recompute_count(), 1, "the first call checks the project");

    // A second call on the same unchanged source reuses the cached analysis.
    let _ = check(Some(&file), None);
    assert_eq!(recompute_count(), 1, "unchanged source must not re-check");

    // A different file and a different tool in the same project also reuse it:
    // the warm cache serves the whole multi-module project.
    let _ = check(Some(&books), None);
    let _ = type_at_position(&books, 0, 0);
    assert_eq!(
        recompute_count(),
        1,
        "the warm cache serves every file and tool"
    );

    // Editing a source file on disk busts the cache.
    std::fs::write(
        &books,
        "module shelf::books\n\npub fn added(): int\n    return 7\n",
    )
    .unwrap();
    let _ = check(Some(&books), None);
    assert_eq!(recompute_count(), 2, "a source change must re-check");
}

#[test]
fn tool_result_shapes_carry_their_pinned_profile_version() {
    // A golden map of each versioned tool result shape to its top-level
    // profile_version. A breaking rename of any shape's version fails here, so an
    // agent pinning a shape has a stable contract.
    reset_project_cache();
    let (_dir, file) = namespace_project();

    let cases: Vec<(&str, Json)> = vec![
        ("mcp.check.v1", check(Some(&file), None)),
        ("mcp.check.v1", check(None, Some("module a"))),
        ("mcp.type_at.v1", type_at_position(&file, 0, 0)),
        (
            "mcp.run.v1",
            run(
                &file,
                Some("shelf::app::run"),
                &[],
                RunMode::Run,
                DEFAULT_RUN_BUDGET,
            ),
        ),
        ("source.completion.v1", complete(&file, 0, 0)),
        (
            "source.namespace.completion.v1",
            namespace_complete(&file, &["books".to_string()]),
        ),
        (
            RESOURCE_SCHEMA_PROFILE_VERSION,
            resource_schema(&file, "Book"),
        ),
        (
            SURFACE_ROUTE_PROFILE_VERSION,
            surface_routes(&file, SurfaceRouteScope::ReadOnly),
        ),
    ];
    for (expected, result) in cases {
        assert_eq!(
            result["profile_version"], expected,
            "tool result shape must pin its profile version: {result}"
        );
    }
}

#[test]
fn run_rejects_malformed_typed_args_before_loading() {
    let result = run(
        Path::new("/nope/project/src/main.mw"),
        Some("app::main"),
        &[json!(1)],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );
    let message = result["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("run argument 0 must be an object"),
        "malformed run args should fail before project loading: {result}"
    );
    assert_eq!(result["output"], "");
    assert_run_contract(&result);
}

#[test]
fn run_rejects_numeric_typed_int_args_before_loading() {
    let result = run(
        Path::new("/nope/project/src/main.mw"),
        Some("app::main"),
        &[json!({ "name": "n", "value": { "kind": "int", "value": 1 } })],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );
    assert_eq!(
        result["diagnostics"][0]["code"], "mcp.run.args",
        "numeric JSON entry ints should be rejected by Marrow protocol admission before project loading: {result}"
    );
}

#[test]
fn run_without_entry_reports_the_session_entry_error() {
    let (_dir, file) = pure_project();
    let result = run(&file, None, &[], RunMode::Run, DEFAULT_RUN_BUDGET);
    let message = result["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("no entry"),
        "missing entry should report the Marrow session error: {result}"
    );
    assert_eq!(result["output"], "");
    assert_run_contract(&result);
}

#[test]
fn run_missing_explicit_entry_uses_marrow_run_error_dto() {
    let (_dir, file) = pure_project();
    let result = run(
        &file,
        Some("shelf::books::missing"),
        &[],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );

    let diagnostic = &result["diagnostics"][0];
    assert_eq!(diagnostic["code"], "run.unknown_function", "{result}");
    assert_ne!(diagnostic["code"], "mcp.run.entry", "{result}");
    assert!(
        diagnostic["kind"].is_string(),
        "entry descriptor errors should use Marrow's run diagnostic DTO: {result}"
    );
    assert!(
        diagnostic["source_span"].is_null(),
        "a user-supplied nonexistent entry is locationless, so its run diagnostic \
         carries a null source span: {result}"
    );
    assert_eq!(result["output"], "", "{result}");
    assert_run_contract(&result);
}

#[test]
fn run_entry_uses_canonical_descriptor_invocation() {
    let (_dir, file) = pure_project();
    let result = run(
        &file,
        Some("shelf::books::shout"),
        &[],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );
    assert_eq!(result["diagnostics"], json!([]), "{result}");
    assert!(
        result["output"].as_str().unwrap().contains("loud"),
        "a zero-argument explicit entry should still run, got {result}"
    );
    assert_run_contract(&result);
}

#[test]
fn run_fault_preserves_output_printed_before_the_fault() {
    let (_dir, file) = pure_project();
    let result = run(
        &file,
        Some("shelf::books::explode"),
        &[],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );

    assert_eq!(
        result["diagnostics"][0]["code"], "run.assertion",
        "{result}"
    );
    assert!(
        result["output"].as_str().unwrap().contains("before fault"),
        "faulting run should keep output emitted before the fault: {result}"
    );
}

#[test]
fn run_uses_fresh_memory_for_shelf_fixture_without_establishing_catalog_identity() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/shelf")
        .canonicalize()
        .expect("the shelf fixture exists");
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src/shelf");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::copy(fixture.join("marrow.json"), dir.path().join("marrow.json")).unwrap();
    std::fs::copy(fixture.join("src/shelf/sample.mw"), src.join("sample.mw")).unwrap();
    let file = src.join("sample.mw");
    let result = run(
        &file,
        Some("shelf::sample::main"),
        &[],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );
    assert_eq!(result["diagnostics"], json!([]), "{result}");
    assert!(
        result["output"].as_str().unwrap().contains("Small Gods"),
        "fresh-memory run should execute the fixture over an in-memory store, got {result}"
    );
    assert!(
        !dir.path().join(CATALOG_FILE_NAME).exists(),
        "mw_run must not establish the fixture's committed lock"
    );
}

#[test]
fn run_accepts_typed_string_arguments() {
    let (_dir, file) = pure_project();
    let result = run(
        &file,
        Some("shelf::books::greet"),
        &[json!({
            "name": "name",
            "value": { "kind": "string", "value": "Ada" }
        })],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );

    assert_eq!(result["diagnostics"], json!([]), "{result}");
    assert!(
        result.get("value").is_none(),
        "mw_run must not expose the old local value field: {result}"
    );
    assert_eq!(result["result"]["kind"], "value", "{result}");
    assert_eq!(
        result["result"]["value"],
        json!({
            "kind": "string",
            "value": "hi Ada",
            "truncated": false,
            "originalBytes": 6
        }),
        "{result}"
    );
    let boundary = &result["runFacts"]["executionBoundary"];
    assert_eq!(boundary["sessionKind"], "run", "{result}");
    assert_eq!(result["runFacts"].get("analysis"), None, "{result}");
    assert_eq!(
        boundary["sourceAnalysisGeneration"]["profileVersion"], "analysis.generation.v1",
        "{result}"
    );
    assert!(
        boundary["sourceAnalysisGeneration"]["sourceIdentity"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "run boundary should carry Marrow's source analysis identity: {result}"
    );
    assert_eq!(boundary["store"]["kind"], "fresh_memory", "{result}");
    assert_eq!(boundary["store"]["stamp"], json!(null), "{result}");
}

#[test]
fn run_check_errors_use_marrow_run_envelope_diagnostics() {
    let (_dir, file) = pure_project();
    std::fs::write(
        &file,
        "\
module shelf::books

pub fn shout(): int
    return \"not an int\"
",
    )
    .unwrap();

    let result = run(
        &file,
        Some("shelf::books::shout"),
        &[],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );

    let diagnostic = &result["diagnostics"][0];
    assert_eq!(result["output"], "", "{result}");
    assert!(result.get("value").is_none(), "{result}");
    assert!(
        diagnostic["kind"].is_string(),
        "check diagnostic should use Marrow's diagnostic DTO: {result}"
    );
    assert_eq!(diagnostic["severity"], "error", "{result}");
    assert!(
        diagnostic["source_span"]["file"]
            .as_str()
            .is_some_and(|path| path.ends_with("books.mw")),
        "diagnostic should carry Marrow source-span metadata: {result}"
    );
}

#[test]
fn run_does_not_touch_the_real_store() {
    let (dir, file) = native_counter_project(1..=1);
    let store_file = dir.path().join("data").join("marrow.redb");
    let before = {
        let store = SealedStore::open(&store_file, AccessMode::Read).expect("open real store");
        store
            .read_commit_metadata()
            .expect("read commit metadata")
            .expect("seed helper stamps the real store")
    };
    let lock_file = dir.path().join(CATALOG_FILE_NAME);
    let lock_before = std::fs::read(&lock_file).expect("seed helper writes committed lock");

    let result = run(
        &file,
        Some("app::counter::show"),
        &[],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );
    assert_eq!(result["diagnostics"], json!([]), "{result}");
    assert!(
        result["output"].as_str().unwrap().contains("absent"),
        "fresh-memory run must not read records from the real store: {result}"
    );

    let after = {
        let store = SealedStore::open(&store_file, AccessMode::Read).expect("reopen real store");
        store
            .read_commit_metadata()
            .expect("read commit metadata")
            .expect("real store remains stamped")
    };
    assert_eq!(
        before, after,
        "fresh-memory run must not advance the real store commit"
    );
    assert_eq!(
        lock_before,
        std::fs::read(&lock_file).expect("fresh-memory run preserves committed lock"),
        "fresh-memory run must not rewrite the committed lock"
    );
}

/// The sandbox conformance oracle for the roadmap S0.2/S3 trust gate: a sandboxed `mw_run` and
/// `mw_test` must leave the real store file byte-for-byte identical, with the same length,
/// modification time, and inode — a strictly stronger check than commit-metadata equality, which
/// a corrupting rewrite that preserved the commit counter could pass. Runs in CI via the test bar.
#[cfg(unix)]
#[test]
fn run_and_test_leave_the_real_store_byte_identical() {
    use std::os::unix::fs::MetadataExt;

    let (dir, file) = native_counter_project(1..=1);
    let store_file = dir.path().join("data").join("marrow.redb");

    fn fingerprint(path: &Path) -> (Vec<u8>, u64, std::time::SystemTime, u64) {
        let bytes = std::fs::read(path).expect("read store file");
        let meta = std::fs::metadata(path).expect("stat store file");
        (
            bytes,
            meta.len(),
            meta.modified().expect("mtime"),
            meta.ino(),
        )
    }

    let before = fingerprint(&store_file);

    let run_result = run(
        &file,
        Some("app::counter::show"),
        &[],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );
    assert_eq!(run_result["diagnostics"], json!([]), "{run_result}");
    assert_eq!(
        fingerprint(&store_file),
        before,
        "sandboxed mw_run perturbed the real store file"
    );

    // A test-mode invocation must also touch nothing, even when it discovers no tests.
    let _ = run(&file, None, &[], RunMode::Test, DEFAULT_RUN_BUDGET);
    assert_eq!(
        fingerprint(&store_file),
        before,
        "sandboxed mw_test perturbed the real store file"
    );
}

#[test]
fn run_reports_marrow_entry_footprint_and_open_mode_facts() {
    let (_dir, file) = native_counter_project(1..=1);

    let result = run(
        &file,
        Some("app::counter::show"),
        &[],
        RunMode::Run,
        DEFAULT_RUN_BUDGET,
    );

    assert_eq!(result["diagnostics"], json!([]), "{result}");
    let facts = &result["runFacts"];
    let generation = &facts["executionBoundary"]["sourceAnalysisGeneration"];
    assert_eq!(facts.get("analysis"), None, "{result}");
    assert_eq!(
        generation["profileVersion"], "analysis.generation.v1",
        "executionBoundary should carry the Marrow analysis generation profile version: {result}"
    );
    assert!(
        generation["sourceIdentity"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "executionBoundary should carry the Marrow analysis source identity: {result}"
    );
    assert!(
        generation["configDigest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "executionBoundary should carry the Marrow config digest: {result}"
    );
    assert!(
        generation["checkedSourceDigest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "executionBoundary should carry the Marrow checked source digest: {result}"
    );
    assert!(
        generation["readOnlyContextDigest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "executionBoundary should carry the Marrow read-only context digest: {result}"
    );
    let accepted_catalog = &generation["acceptedCatalog"];
    assert!(
        accepted_catalog["epoch"].is_u64(),
        "committed native fixture should carry accepted catalog epoch: {result}"
    );
    assert!(
        accepted_catalog["digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "committed native fixture should carry accepted catalog digest: {result}"
    );
    assert!(
        generation["proposalCatalog"].is_null()
            || (generation["proposalCatalog"]["epoch"].is_u64()
                && generation["proposalCatalog"]["digest"]
                    .as_str()
                    .is_some_and(|digest| digest.starts_with("sha256:"))),
        "proposal catalog should be null or carry epoch and digest: {result}"
    );
    assert_eq!(facts["entry"]["requestedName"], "app::counter::show");
    assert_eq!(facts["entry"]["canonicalName"], "app::counter::show");
    assert!(
        facts["entry"]["entryTag"]
            .as_str()
            .is_some_and(|tag| tag.starts_with("sha256:")),
        "entry descriptor tag should come from Marrow: {result}"
    );
    assert!(
        facts["entry"]["acceptedCatalogEpoch"].is_u64(),
        "committed native fixture should carry accepted catalog identity: {result}"
    );
    assert!(
        facts["entry"]["sourceDigest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "entry descriptor should carry the source digest: {result}"
    );
    assert!(
        facts["entry"]["readOnlyContextDigest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "entry descriptor should carry the read-only context digest: {result}"
    );
    assert_eq!(facts["storeOpenMode"], "read_only", "{result}");
    assert_eq!(facts["footprint"]["entry"], "app::counter::show");
    assert_eq!(facts["footprint"]["workShape"], "read_only");
    assert_eq!(facts["footprint"]["writeEffectsReachable"], false);
    assert_eq!(
        facts["footprint"]["storesRead"],
        json!(["app::counter::^counter"])
    );
    assert_eq!(facts["footprint"]["storesWritten"], json!([]));
    assert_eq!(facts["footprint"]["indexesTouched"], json!([]));
    assert_eq!(facts["costShape"]["entry"], "app::counter::show");
    assert_eq!(facts["costShape"]["workShape"], "read_only");
    assert_eq!(facts["costShape"]["pointReads"], 1);
    assert_eq!(facts["costShape"]["rangeScans"], 0);
    assert_eq!(facts["costShape"]["writes"], 0);
    assert_eq!(facts["costShape"]["indexEntryTouches"], 0);
    assert_eq!(facts["costShape"]["commitPoints"], 0);
    assert_run_contract(&result);
}

#[cfg(unix)]
#[test]
fn test_mode_does_not_inspect_the_real_store() {
    use std::os::unix::fs::PermissionsExt;

    let (dir, file) = native_counter_project(1..=1);
    let tests_dir = dir.path().join("tests");
    std::fs::create_dir_all(&tests_dir).unwrap();
    std::fs::write(
        tests_dir.join("smoke_test.mw"),
        "pub fn smoke()\n    std::assert::isTrue(true)\n",
    )
    .unwrap();
    let store_file = dir.path().join("data").join("marrow.redb");
    let mut unreadable = std::fs::metadata(&store_file)
        .expect("fixture seeds a native store")
        .permissions();
    unreadable.set_mode(0o0);
    std::fs::set_permissions(&store_file, unreadable).expect("make native store unreadable");

    let result = run(&file, None, &[], RunMode::Test, DEFAULT_RUN_BUDGET);

    assert_eq!(result["diagnostics"], json!([]), "{result}");
    let tests = result["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 1, "{result}");
    assert_eq!(tests[0]["name"], "tests::smoke_test::smoke");
    assert_eq!(tests[0]["outcome"], "passed");
    assert_eq!(
        result["executionBoundary"]["sessionKind"], "test",
        "{result}"
    );
    assert_eq!(
        result["executionBoundary"]["sourceAnalysisGeneration"]["profileVersion"],
        "analysis.generation.v1",
        "{result}"
    );
    assert!(
        result["executionBoundary"]["sourceAnalysisGeneration"]["sourceIdentity"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "test boundary should carry Marrow's source analysis identity: {result}"
    );
    assert_eq!(
        result["executionBoundary"]["store"]["kind"], "test_memory",
        "{result}"
    );
    assert_eq!(
        result["executionBoundary"]["store"]["stamp"],
        json!(null),
        "{result}"
    );
    assert_run_contract(&result);
}

#[test]
fn test_mode_runs_each_test_over_a_fresh_store() {
    let (dir, file) = pure_project();
    let tests_dir = dir.path().join("tests");
    std::fs::create_dir_all(&tests_dir).unwrap();
    std::fs::write(
        tests_dir.join("books_test.mw"),
        "\
use shelf::books

pub fn doubles()
    std::assert::isTrue(books::double(2) == 4)

pub fn fails()
    std::assert::isTrue(books::double(2) == 5)
",
    )
    .unwrap();
    let result = run(&file, None, &[], RunMode::Test, DEFAULT_RUN_BUDGET);
    let tests = result["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 2, "two test functions discovered: {result}");
    let doubles = tests
        .iter()
        .find(|t| t["name"].as_str().unwrap().ends_with("doubles"))
        .unwrap();
    assert_eq!(doubles["outcome"], "passed");
    let fails = tests
        .iter()
        .find(|t| t["name"].as_str().unwrap().ends_with("fails"))
        .unwrap();
    assert_eq!(
        fails["outcome"], "failed",
        "an assertion failure is a failed test: {fails}"
    );
    assert_eq!(
        result["executionBoundary"]["sessionKind"], "test",
        "{result}"
    );
    assert_eq!(
        result["executionBoundary"]["sourceAnalysisGeneration"]["profileVersion"],
        "analysis.generation.v1",
        "{result}"
    );
    assert_eq!(
        result["executionBoundary"]["store"]["kind"], "test_memory",
        "{result}"
    );
    assert_eq!(
        result["executionBoundary"]["store"]["stamp"],
        json!(null),
        "{result}"
    );
    assert_run_contract(&result);
}

#[test]
fn run_test_mode_rejects_args() {
    let (_dir, file) = pure_project();
    let result = run(
        &file,
        None,
        &[json!({ "name": "value", "value": { "kind": "int", "value": "1" } })],
        RunMode::Test,
        DEFAULT_RUN_BUDGET,
    );
    assert_eq!(
        result["diagnostics"][0]["code"], "mcp.run.args",
        "test mode args should be rejected before running tests: {result}"
    );
}

#[test]
fn data_tools_refuse_when_not_enabled() {
    let (_dir, file) = project();
    let roots = saved_roots(&file, None);
    assert_eq!(roots["available"], false);
    assert_eq!(roots["dataAccess"], "disabled");
    assert!(
        roots.get("data_view_boundary").is_none(),
        "disabled data replies must not carry a fake boundary: {roots}"
    );
    assert_saved_roots_contract(&roots);
    let children = data_children(
        Path::new("/nope/project/src/main.mw"),
        crate::data_explorer::DataChildrenRequest {
            segments: vec![sample_root_segment()],
            limit: 1,
            cursor: None,
        },
        None,
    );
    assert_eq!(children["dataAccess"], "disabled");
    assert!(
        children.get("data_view_boundary").is_none(),
        "disabled data replies must not carry a fake boundary: {children}"
    );
    assert_data_children_contract(&children);

    let read = data_read(
        Path::new("/nope/project/src/main.mw"),
        crate::data_explorer::DataReadRequest {
            segments: vec![sample_root_segment()],
            preview_limit: None,
        },
        None,
    );
    assert_eq!(read["dataAccess"], "disabled");
    assert!(
        read.get("data_view_boundary").is_none(),
        "disabled data replies must not carry a fake boundary: {read}"
    );
    assert_data_read_contract(&read);
}

#[test]
fn saved_roots_return_snapshot_metadata_when_enabled() {
    let (_dir, file) = native_counter_project(1..=1);
    let result = saved_roots(&file, granted_read());
    let counter_root = saved_data_root_segment(&file, "counter");

    assert_eq!(result["available"], true, "{result}");
    assert_eq!(
        result["roots"],
        json!([
            {
                "segment": counter_root,
                "label": "counter"
            }
        ]),
        "{result}"
    );
    assert_store_snapshot(&result["store_snapshot"]);
    assert_data_view_boundary(&result["data_view_boundary"]);
    assert_saved_roots_contract(&result);
}

#[test]
fn data_tools_refuse_unstamped_records_even_with_committed_lock() {
    let (_dir, file) = native_counter_project_with_unstamped_records(1..=1);

    let roots = saved_roots(&file, granted_read());
    assert_eq!(roots["available"], false, "{roots}");
    assert_eq!(roots["roots"], json!([]), "{roots}");
    assert!(
        roots.get("data_view_boundary").is_none(),
        "unavailable data replies must not carry a fake boundary: {roots}"
    );
    assert_saved_roots_contract(&roots);

    let children = data_children(
        &file,
        crate::data_explorer::DataChildrenRequest {
            segments: vec![sample_root_segment()],
            limit: 10,
            cursor: None,
        },
        granted_read(),
    );

    assert_eq!(children["available"], false, "{children}");
    assert_eq!(children["children"], json!([]), "{children}");
    assert_eq!(children["truncated"], false, "{children}");
    assert_eq!(children["cursor"], Json::Null, "{children}");
    assert_eq!(children["store_snapshot"], Json::Null, "{children}");
    assert!(
        children.get("data_view_boundary").is_none(),
        "unavailable data replies must not carry a fake boundary: {children}"
    );
    assert_data_children_contract(&children);
}

#[test]
fn data_children_returns_paged_typed_segments_when_enabled() {
    let (_dir, file) = native_counter_project(1..=3);
    let counter_root = saved_data_root_segment(&file, "counter");

    let result = data_children(
        &file,
        crate::data_explorer::DataChildrenRequest {
            segments: vec![counter_root],
            limit: 2,
            cursor: None,
        },
        granted_read(),
    );

    assert_eq!(
        result["children"],
        json!([
            {
                "segment": { "kind": "key", "value": { "kind": "int", "value": 1 } },
                "label": "(1)"
            },
            {
                "segment": { "kind": "key", "value": { "kind": "int", "value": 2 } },
                "label": "(2)"
            },
        ]),
        "{result}"
    );
    assert_eq!(result["truncated"], true, "{result}");
    assert_eq!(
        result["cursor"],
        json!({ "kind": "int", "value": 2 }),
        "{result}"
    );
    assert_store_snapshot(&result["store_snapshot"]);
    assert_data_view_boundary(&result["data_view_boundary"]);
    assert_data_children_contract(&result);
}

#[test]
fn data_read_returns_bounded_value_preview_when_enabled() {
    let (_dir, file) = native_counter_project_with_value(42);
    let counter_root = saved_data_root_segment(&file, "counter");
    let value_field = saved_data_field_segment(&file, "counter", "value");

    let result = data_read(
        &file,
        crate::data_explorer::DataReadRequest {
            segments: vec![
                counter_root,
                crate::data_explorer::DataPathSegmentDto::Key {
                    value: crate::data_explorer::DataKeyDto::Int(1),
                },
                value_field,
            ],
            preview_limit: Some(32),
        },
        granted_read(),
    );

    assert_eq!(result["available"], true, "{result}");
    assert_eq!(result["presence"], "value_only", "{result}");
    assert_eq!(result["value"], "42", "{result}");
    assert_eq!(result["value_truncated"], false, "{result}");
    assert_store_snapshot(&result["store_snapshot"]);
    assert_data_view_boundary(&result["data_view_boundary"]);
    assert_data_read_contract(&result);
}

#[test]
fn data_children_returns_empty_page_for_absent_members() {
    let (_dir, file) = native_counter_project(1..=1);
    let counter_root = saved_data_root_segment(&file, "counter");

    let result = data_children(
        &file,
        crate::data_explorer::DataChildrenRequest {
            segments: vec![
                counter_root,
                crate::data_explorer::DataPathSegmentDto::Key {
                    value: crate::data_explorer::DataKeyDto::Int(1),
                },
            ],
            limit: 1,
            cursor: None,
        },
        granted_read(),
    );

    assert_eq!(result["available"], true, "{result}");
    assert_eq!(result["children"], json!([]), "{result}");
    assert_eq!(result["truncated"], false, "{result}");
    assert_eq!(result["cursor"], Json::Null, "{result}");
    assert_store_snapshot(&result["store_snapshot"]);
    assert_data_children_contract(&result);
}

#[test]
fn data_children_returns_empty_page_for_absent_keyless_root() {
    let (_dir, file) = native_settings_project();
    let settings_root = saved_data_root_segment(&file, "settings");

    let result = data_children(
        &file,
        crate::data_explorer::DataChildrenRequest {
            segments: vec![settings_root],
            limit: 1,
            cursor: None,
        },
        granted_read(),
    );

    assert_eq!(result["available"], true, "{result}");
    assert_eq!(result["children"], json!([]), "{result}");
    assert_eq!(result["truncated"], false, "{result}");
    assert_eq!(result["cursor"], Json::Null, "{result}");
    assert_store_snapshot(&result["store_snapshot"]);
    assert_data_children_contract(&result);
}
