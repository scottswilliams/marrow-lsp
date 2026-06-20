use super::*;

use marrow_check::test_support::{commit_then_check, member_catalog_id, root_place, store_id_of};
use marrow_store::key::SavedKey;
use marrow_store::tree::{DataPathSegment, StoreUid, TreeStore};
use marrow_store::value::{SavedValue, encode_value};

fn assert_contract(result: &Json, status: &str, description: &str, missing_facts: &[&str]) {
    let contract = &result["contract"];
    assert_eq!(contract["status"], status, "contract status for {result}");
    assert_eq!(
        contract["stableProductionApi"], false,
        "stableProductionApi must be false for {result}"
    );
    assert_eq!(
        contract["description"], description,
        "contract description for {result}"
    );
    let actual: Vec<&str> = contract["missingFacts"]
        .as_array()
        .unwrap_or_else(|| panic!("contract missingFacts must be an array: {result}"))
        .iter()
        .map(|fact| fact.as_str().expect("missing fact string"))
        .collect();
    assert_eq!(actual, missing_facts, "missing facts for {result}");
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

fn assert_store_snapshot(snapshot: &Json) {
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

fn assert_checked_snapshot(snapshot: &Json) {
    assert_eq!(
        snapshot["store_uid"],
        Json::Null,
        "snapshot should not fabricate a store UID: {snapshot}"
    );
    assert_eq!(
        snapshot["catalog_digest"],
        Json::Null,
        "snapshot should not fabricate a catalog digest: {snapshot}"
    );
    assert_eq!(
        snapshot["commit"],
        Json::Null,
        "snapshot should not fabricate commit metadata: {snapshot}"
    );
    assert!(
        snapshot["checked_source_digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "snapshot should carry the checked program source digest: {snapshot}"
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

fn position_in_file(file: &Path, needle: &str) -> Position {
    let source = std::fs::read_to_string(file).unwrap();
    let offset = source
        .find(needle)
        .unwrap_or_else(|| panic!("fixture source should contain {needle:?}"));
    crate::positions::LineIndex::new(source).position(offset)
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

    let program = commit_then_check(root).unwrap();
    let file = src.join("counter.mw");
    let place = root_place(&program, "counter").unwrap();
    let store_id = store_id_of(&place).unwrap();
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let store = TreeStore::open(&data_dir.join("marrow.redb")).unwrap();
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

fn native_counter_project_with_value(value: i64) -> (tempfile::TempDir, PathBuf) {
    let (dir, file) = native_counter_project([1]);
    let root = dir.path();
    let program = commit_then_check(root).unwrap();
    let place = root_place(&program, "counter").unwrap();
    let store_id = store_id_of(&place).unwrap();
    let value_id = member_catalog_id(&place, "value").unwrap();
    let path = vec![DataPathSegment::Member(
        marrow_store::cell::CatalogId::new(value_id).unwrap(),
    )];
    let store = TreeStore::open(&root.join("data/marrow.redb")).unwrap();
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

    commit_then_check(root).unwrap();
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    drop(TreeStore::open(&data_dir.join("marrow.redb")).unwrap());

    (dir, src.join("settings.mw"))
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
    commit_then_check(dir.path()).expect("commit surface catalog");
    (dir, file)
}

fn point_read_operation(file: &Path) -> Json {
    let (workspace, _) = load_project(file, None).expect("surface project checks");
    let program = workspace.program().expect("checked program");
    let abi = marrow_json::surface::SurfaceAbiJson::from_program(program);
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
    let (workspace, _) = load_project(file, None).expect("surface project checks");
    let program = workspace.program().expect("checked program");
    let abi = marrow_json::surface::SurfaceAbiJson::from_program(program);
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
    let (workspace, _) = load_project(file, None).expect("surface project checks");
    let program = workspace.program().expect("checked program");
    let abi = marrow_json::surface::SurfaceAbiJson::from_program(program);
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
    assert_contract(
        &result,
        "presentation-only",
        "development helper",
        &["canonical completion-context facts"],
    );
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

    let result = surface_read(Path::new("/nope/project/src/main.mw"), request, false)
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

    let result = surface_write(Path::new("/nope/project/src/main.mw"), request, false)
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
    let before =
        surface_read(&file, point_read_operation(&file), true).expect("valid read operation");

    let result = surface_write(&file, update_author_operation(&file, "Brian Herbert"), true)
        .expect("valid update operation");
    let after =
        surface_read(&file, point_read_operation(&file), true).expect("valid read operation");

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
fn surface_write_executes_canonical_action_request_and_returns_canonical_errors() {
    let (_dir, file) = native_surface_project();

    let result = surface_write(&file, retitle_action_operation(&file, "Dune MCP"), true)
        .expect("valid action operation");
    let after =
        surface_read(&file, point_read_operation(&file), true).expect("valid read operation");

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
    let mismatch = surface_write(&file, wrong_body, true).expect("valid operation DTO");
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

    let result =
        surface_read(&file, point_read_operation(&file), true).expect("valid operation request");

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
    let (workspace, _) = load_project(&file, None).expect("surface project checks");
    let abi = marrow_json::surface::SurfaceAbiJson::from_program(
        workspace.program().expect("checked program"),
    );
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

    let result = surface_read(&file, request, true).expect("write operation request decodes");

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
    let result = run(&file, Some("shelf::books::shout"), &[], RunMode::Run);
    assert_eq!(result["diagnostics"].as_array().unwrap().len(), 0);
    assert!(
        result["output"].as_str().unwrap().contains("loud"),
        "zero-argument run should execute: {result}"
    );
    assert_contract(
        &result,
        "presentation-only",
        "sandboxed execution helper",
        &[
            "transitive effect facts",
            "durable-scope facts",
            "transaction facts",
            "runtime generation facts",
        ],
    );
}

#[test]
fn run_rejects_malformed_typed_args_before_loading() {
    let result = run(
        Path::new("/nope/project/src/main.mw"),
        Some("app::main"),
        &[json!(1)],
        RunMode::Run,
    );
    let message = result["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("run argument 0 must be an object"),
        "malformed run args should fail before project loading: {result}"
    );
    assert_eq!(result["output"], "");
    assert_contract(
        &result,
        "presentation-only",
        "sandboxed execution helper",
        &[
            "transitive effect facts",
            "durable-scope facts",
            "transaction facts",
            "runtime generation facts",
        ],
    );
}

#[test]
fn run_rejects_numeric_typed_int_args_before_loading() {
    let result = run(
        Path::new("/nope/project/src/main.mw"),
        Some("app::main"),
        &[json!({ "name": "n", "value": { "kind": "int", "value": 1 } })],
        RunMode::Run,
    );
    assert_eq!(
        result["diagnostics"][0]["code"], "mcp.run.args",
        "numeric JSON entry ints should be rejected by Marrow protocol admission before project loading: {result}"
    );
}

#[test]
fn run_without_entry_reports_the_session_entry_error() {
    let (_dir, file) = pure_project();
    let result = run(&file, None, &[], RunMode::Run);
    let message = result["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("no entry"),
        "missing entry should report the Marrow session error: {result}"
    );
    assert_eq!(result["output"], "");
    assert_contract(
        &result,
        "presentation-only",
        "sandboxed execution helper",
        &[
            "transitive effect facts",
            "durable-scope facts",
            "transaction facts",
            "runtime generation facts",
        ],
    );
}

#[test]
fn run_value_json_summarizes_local_trees() {
    assert_eq!(
        value_to_json(Value::LocalTree(Vec::new())),
        json!({ "tree": 0 })
    );
}

#[test]
fn run_value_json_caps_large_sequences() {
    let value = Value::Sequence((0..300).map(Value::Int).collect());
    let rendered = value_to_json(value);
    let items = rendered["sequence"]
        .as_array()
        .unwrap_or_else(|| panic!("large sequences render as a bounded summary: {rendered}"));

    assert_eq!(rendered["truncated"], true, "{rendered}");
    assert!(items.len() < 300, "{rendered}");
}

#[test]
fn run_value_json_caps_large_strings() {
    let rendered = value_to_json(Value::Str("x".repeat(20_000)));

    assert_eq!(rendered["truncated"], true, "{rendered}");
    assert!(
        rendered["string"].as_str().unwrap().len() < 20_000,
        "{rendered}"
    );
}

#[test]
fn run_value_json_caps_large_resources() {
    let value = Value::Resource(
        (0..300)
            .map(|index| (format!("field{index}"), Value::Int(index)))
            .collect(),
    );
    let rendered = value_to_json(value);
    let fields = rendered["resource"]
        .as_array()
        .unwrap_or_else(|| panic!("large resources render as a bounded summary: {rendered}"));

    assert_eq!(rendered["truncated"], true, "{rendered}");
    assert!(fields.len() < 300, "{rendered}");
}

#[test]
fn run_value_json_caps_large_resource_field_names() {
    let rendered = value_to_json(Value::Resource(vec![("x".repeat(20_000), Value::Int(1))]));
    let fields = rendered["resource"]
        .as_array()
        .unwrap_or_else(|| panic!("long resource names render as a bounded summary: {rendered}"));

    assert_eq!(rendered["truncated"], true, "{rendered}");
    assert_eq!(fields[0]["nameTruncated"], true, "{rendered}");
    assert!(
        fields[0]["name"].as_str().unwrap().len() < 20_000,
        "{rendered}"
    );
}

#[test]
fn run_entry_uses_canonical_descriptor_invocation() {
    let (_dir, file) = pure_project();
    let result = run(&file, Some("shelf::books::shout"), &[], RunMode::Run);
    assert_eq!(result["diagnostics"], json!([]), "{result}");
    assert!(
        result["output"].as_str().unwrap().contains("loud"),
        "a zero-argument explicit entry should still run, got {result}"
    );
    assert_contract(
        &result,
        "presentation-only",
        "sandboxed execution helper",
        &[
            "transitive effect facts",
            "durable-scope facts",
            "transaction facts",
            "runtime generation facts",
        ],
    );
}

#[test]
fn run_fault_preserves_output_printed_before_the_fault() {
    let (_dir, file) = pure_project();
    let result = run(&file, Some("shelf::books::explode"), &[], RunMode::Run);

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
    let result = run(&file, Some("shelf::sample::main"), &[], RunMode::Run);
    assert_eq!(result["diagnostics"], json!([]), "{result}");
    assert!(
        result["output"].as_str().unwrap().contains("Small Gods"),
        "fresh-memory run should execute the fixture over an in-memory store, got {result}"
    );
    assert!(
        !dir.path().join("marrow.catalog.json").exists(),
        "mw_run must not establish the fixture's accepted catalog"
    );
}

#[test]
fn output_truncation_preserves_utf8_boundaries() {
    let output = "€".repeat(OUTPUT_CAP);
    let truncated = truncate(output);

    assert!(truncated.ends_with("…output truncated…"));
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
    );

    assert_eq!(result["diagnostics"], json!([]), "{result}");
    assert_eq!(result["value"], "hi Ada", "{result}");
}

#[test]
fn run_does_not_touch_the_real_store() {
    let (dir, file) = native_counter_project(1..=1);
    let store_file = dir.path().join("data").join("marrow.redb");
    let before = {
        let store = TreeStore::open_read_only(&store_file).expect("open real store");
        store
            .read_commit_metadata()
            .expect("read commit metadata")
            .expect("seed helper stamps the real store")
    };
    let catalog_file = dir.path().join("marrow.catalog.json");
    let catalog_before = std::fs::read(&catalog_file).expect("seed helper writes catalog");

    let result = run(&file, Some("app::counter::show"), &[], RunMode::Run);
    assert_eq!(result["diagnostics"], json!([]), "{result}");
    assert!(
        result["output"].as_str().unwrap().contains("absent"),
        "fresh-memory run must not read records from the real store: {result}"
    );

    let after = {
        let store = TreeStore::open_read_only(&store_file).expect("reopen real store");
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
        catalog_before,
        std::fs::read(&catalog_file).expect("fresh-memory run preserves catalog"),
        "fresh-memory run must not rewrite the accepted catalog artifact"
    );
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
    let result = run(&file, None, &[], RunMode::Test);
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
    assert_contract(
        &result,
        "presentation-only",
        "sandboxed execution helper",
        &[
            "transitive effect facts",
            "durable-scope facts",
            "transaction facts",
            "runtime generation facts",
        ],
    );
}

#[test]
fn run_test_mode_rejects_args() {
    let (_dir, file) = pure_project();
    let result = run(
        &file,
        None,
        &[json!({ "name": "value", "value": { "kind": "int", "value": "1" } })],
        RunMode::Test,
    );
    assert_eq!(
        result["diagnostics"][0]["code"], "mcp.run.args",
        "test mode args should be rejected before running tests: {result}"
    );
}

#[test]
fn data_tools_refuse_when_not_enabled() {
    let (_dir, file) = project();
    let roots = saved_roots(&file, false);
    assert_eq!(roots["available"], false);
    assert_eq!(roots["dataAccess"], "disabled");
    assert_contract(
        &roots,
        "presentation-only",
        "saved-root listing helper",
        &[
            "catalog-bound saved-root identity",
            "versioned source/store generation",
            "watch/refresh facts",
            "integrity and repair facts",
            "serve/attach data boundaries",
        ],
    );
    let integrity = data_integrity(&file, false);
    assert_eq!(integrity["dataAccess"], "disabled");
    assert_eq!(integrity["store_snapshot"], Json::Null);
    assert_contract(
        &integrity,
        "presentation-only",
        "debug/admin advisory",
        &[
            "source/store compatibility verdicts",
            "drift witnesses",
            "typed repair facts",
            "stable production integrity DTOs",
        ],
    );

    let children = data_children(
        Path::new("/nope/project/src/main.mw"),
        crate::data_explorer::DataChildrenRequest {
            segments: vec![crate::data_explorer::DataPathSegmentDto::Root(
                "counter".into(),
            )],
            limit: 1,
            cursor: None,
        },
        false,
    );
    assert_eq!(children["dataAccess"], "disabled");
    assert_contract(
        &children,
        "presentation-only",
        "bounded typed data helper",
        &[
            "catalog-bound saved-place identity",
            "versioned source/store generation",
            "watch/refresh facts",
            "integrity and repair facts",
            "serve/attach data boundaries",
        ],
    );

    let read = data_read(
        Path::new("/nope/project/src/main.mw"),
        crate::data_explorer::DataReadRequest {
            segments: vec![crate::data_explorer::DataPathSegmentDto::Root(
                "counter".into(),
            )],
            preview_limit: None,
        },
        false,
    );
    assert_eq!(read["dataAccess"], "disabled");
    assert_contract(
        &read,
        "presentation-only",
        "data value preview helper",
        &[
            "catalog-bound saved-place identity",
            "versioned source/store generation",
            "bounded presence facts",
            "watch/refresh facts",
            "integrity and repair facts",
            "serve/attach data boundaries",
        ],
    );
}

#[test]
fn saved_roots_return_snapshot_metadata_when_enabled() {
    let (_dir, file) = native_counter_project(1..=1);
    let result = saved_roots(&file, true);

    assert_eq!(result["available"], true, "{result}");
    assert_eq!(
        result["roots"],
        json!([
            {
                "segment": { "kind": "root", "value": "counter" },
                "label": "counter"
            }
        ]),
        "{result}"
    );
    assert_store_snapshot(&result["store_snapshot"]);
    assert_contract(
        &result,
        "presentation-only",
        "saved-root listing helper",
        &[
            "catalog-bound saved-root identity",
            "versioned source/store generation",
            "watch/refresh facts",
            "integrity and repair facts",
            "serve/attach data boundaries",
        ],
    );
}

#[test]
fn data_integrity_returns_snapshot_metadata_when_enabled() {
    let (_dir, file) = native_counter_project(1..=1);

    let result = data_integrity(&file, true);

    assert_eq!(result["available"], true, "{result}");
    assert_store_snapshot(&result["store_snapshot"]);
    assert!(
        result["scanned"]
            .as_u64()
            .is_some_and(|scanned| scanned > 0)
    );
    let codes = result["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .map(|finding| finding["code"].as_str().expect("finding code"))
        .collect::<Vec<_>>();
    assert!(codes.contains(&"data.incomplete"), "{result}");
    assert_contract(
        &result,
        "presentation-only",
        "debug/admin advisory",
        &[
            "source/store compatibility verdicts",
            "drift witnesses",
            "typed repair facts",
            "stable production integrity DTOs",
        ],
    );
}

#[test]
fn data_children_returns_paged_typed_segments_when_enabled() {
    let (_dir, file) = native_counter_project(1..=3);

    let result = data_children(
        &file,
        crate::data_explorer::DataChildrenRequest {
            segments: vec![crate::data_explorer::DataPathSegmentDto::Root(
                "counter".into(),
            )],
            limit: 2,
            cursor: None,
        },
        true,
    );

    assert_eq!(
        result["children"],
        json!([
            { "kind": "key", "value": { "kind": "int", "value": 1 } },
            { "kind": "key", "value": { "kind": "int", "value": 2 } },
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
    assert_contract(
        &result,
        "presentation-only",
        "bounded typed data helper",
        &[
            "catalog-bound saved-place identity",
            "versioned source/store generation",
            "watch/refresh facts",
            "integrity and repair facts",
            "serve/attach data boundaries",
        ],
    );
}

#[test]
fn data_read_returns_bounded_value_preview_when_enabled() {
    let (_dir, file) = native_counter_project_with_value(42);

    let result = data_read(
        &file,
        crate::data_explorer::DataReadRequest {
            segments: vec![
                crate::data_explorer::DataPathSegmentDto::Root("counter".into()),
                crate::data_explorer::DataPathSegmentDto::Key(
                    crate::data_explorer::DataKeyDto::Int(1),
                ),
                crate::data_explorer::DataPathSegmentDto::Field("value".into()),
            ],
            preview_limit: Some(32),
        },
        true,
    );

    assert_eq!(result["available"], true, "{result}");
    assert_eq!(result["presence"], "value_only", "{result}");
    assert_eq!(result["value"], "42", "{result}");
    assert_eq!(result["value_truncated"], false, "{result}");
    assert_store_snapshot(&result["store_snapshot"]);
    assert_contract(
        &result,
        "presentation-only",
        "data value preview helper",
        &[
            "catalog-bound saved-place identity",
            "versioned source/store generation",
            "bounded presence facts",
            "watch/refresh facts",
            "integrity and repair facts",
            "serve/attach data boundaries",
        ],
    );
}

#[test]
fn data_children_returns_empty_page_for_absent_members() {
    let (_dir, file) = native_counter_project(1..=1);

    let result = data_children(
        &file,
        crate::data_explorer::DataChildrenRequest {
            segments: vec![
                crate::data_explorer::DataPathSegmentDto::Root("counter".into()),
                crate::data_explorer::DataPathSegmentDto::Key(
                    crate::data_explorer::DataKeyDto::Int(1),
                ),
            ],
            limit: 1,
            cursor: None,
        },
        true,
    );

    assert_eq!(result["available"], true, "{result}");
    assert_eq!(result["children"], json!([]), "{result}");
    assert_eq!(result["truncated"], false, "{result}");
    assert_eq!(result["cursor"], Json::Null, "{result}");
    assert_store_snapshot(&result["store_snapshot"]);
    assert_contract(
        &result,
        "presentation-only",
        "bounded typed data helper",
        &[
            "catalog-bound saved-place identity",
            "versioned source/store generation",
            "watch/refresh facts",
            "integrity and repair facts",
            "serve/attach data boundaries",
        ],
    );
}

#[test]
fn data_children_returns_empty_page_for_absent_keyless_root() {
    let (_dir, file) = native_settings_project();

    let result = data_children(
        &file,
        crate::data_explorer::DataChildrenRequest {
            segments: vec![crate::data_explorer::DataPathSegmentDto::Root(
                "settings".into(),
            )],
            limit: 1,
            cursor: None,
        },
        true,
    );

    assert_eq!(result["available"], true, "{result}");
    assert_eq!(result["children"], json!([]), "{result}");
    assert_eq!(result["truncated"], false, "{result}");
    assert_eq!(result["cursor"], Json::Null, "{result}");
    assert_checked_snapshot(&result["store_snapshot"]);
    assert_contract(
        &result,
        "presentation-only",
        "bounded typed data helper",
        &[
            "catalog-bound saved-place identity",
            "versioned source/store generation",
            "watch/refresh facts",
            "integrity and repair facts",
            "serve/attach data boundaries",
        ],
    );
}
