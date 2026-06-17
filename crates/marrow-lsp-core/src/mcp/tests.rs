use super::*;

use marrow_check::test_support::{commit_then_check, root_place, store_id_of};
use marrow_store::key::SavedKey;
use marrow_store::tree::TreeStore;

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
    for id in ids {
        store
            .write_record_presence(&store_id, &[SavedKey::Int(id)])
            .unwrap();
    }
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
    // `    return n * 2` is line 10 (zero-based); `    return ` is 11 chars, so
    // the `n` parameter use sits at character 11.
    let result = type_at_position(&file, 10, 11);
    assert_eq!(
        result["type"], "int",
        "the `n` parameter use is an int: {result}"
    );
}

#[test]
fn complete_in_a_function_body_lists_locals_and_keywords() {
    let (_dir, file) = project();
    // A bare position inside `double`'s body (line 10, after `    return `) lists
    // in-scope names and keywords — proving the schema-backed program loaded and
    // the classifier ran against the file's own text.
    let result = complete(&file, 10, 11);
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
    let resources = result["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1, "one Book resource: {result}");
    let book = &resources[0];
    assert_eq!(book["name"], "Book");
    let stores = book["stores"].as_array().unwrap();
    assert_eq!(stores.len(), 1);
    let store = &stores[0];
    assert_eq!(store["root"], "books");
    assert_eq!(store["resource"], "Book");
    assert_eq!(store["identityKeys"][0]["name"], "id");
    assert_eq!(store["identityKeys"][0]["type"], "int");
    // `title` is a required string field; `tags` is a keyed leaf; `byTitle` an index.
    let members = book["members"].as_array().unwrap();
    assert!(
        members
            .iter()
            .any(|m| m["name"] == "title" && m["kind"] == "field" && m["type"] == "string")
    );
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
            .any(|i| i["name"] == "byTitle")
    );
    assert_contract(
        &result,
        "presentation-only",
        "development helper",
        &[
            "catalog-bound resource/store/member identity",
            "presence/default facts",
            "typed protocol DTOs",
        ],
    );
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
        result["diagnostics"][0]["code"], "mcp.resourceSchema.identity",
        "{result}"
    );
    assert_contract(
        &result,
        "presentation-only",
        "development helper",
        &[
            "catalog-bound resource/store/member identity",
            "presence/default facts",
            "typed protocol DTOs",
        ],
    );
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
            "canonical function-entry facts",
            "transitive effect facts",
            "durable-scope facts",
            "transaction facts",
            "runtime generation facts",
            "typed run protocol DTOs",
        ],
    );
}

#[test]
fn run_blocks_non_empty_args_before_loading() {
    let result = run(
        Path::new("/nope/project/src/main.mw"),
        Some("app::main"),
        &[json!(1)],
        RunMode::Run,
    );
    let message = result["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("typed run argument facts from Marrow"),
        "non-empty run args should be blocked before project loading: {result}"
    );
    assert_eq!(result["output"], "");
    assert_contract(
        &result,
        "presentation-only",
        "sandboxed execution helper",
        &[
            "canonical function-entry facts",
            "transitive effect facts",
            "durable-scope facts",
            "transaction facts",
            "runtime generation facts",
            "typed run protocol DTOs",
        ],
    );
}

#[test]
fn run_without_entry_frames_the_blocked_entry_contract() {
    let (_dir, file) = pure_project();
    let result = run(&file, None, &[], RunMode::Run);
    let message = result["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("blocked-on-marrow entry string"),
        "missing entry should frame entry as blocked: {result}"
    );
    assert!(
        message.contains("canonical function-entry facts"),
        "missing entry should name the blocking facts: {result}"
    );
    assert!(
        message.contains("not a stable production entry API"),
        "missing entry should not imply a stable entry API: {result}"
    );
    assert_eq!(result["output"], "");
    assert_contract(
        &result,
        "presentation-only",
        "sandboxed execution helper",
        &[
            "canonical function-entry facts",
            "transitive effect facts",
            "durable-scope facts",
            "transaction facts",
            "runtime generation facts",
            "typed run protocol DTOs",
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
fn run_contract_marks_entry_string_as_presentation_only() {
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
            "canonical function-entry facts",
            "transitive effect facts",
            "durable-scope facts",
            "transaction facts",
            "runtime generation facts",
            "typed run protocol DTOs",
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
fn run_blocks_shelf_fixture_until_catalog_identity_is_established() {
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
    assert_eq!(
        result["diagnostics"][0]["code"], "mcp.run.catalog",
        "{result}"
    );
    assert!(
        result["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("accepted catalog identity"),
        "durable fixture should block on catalog identity, got {result}"
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
fn run_blocks_string_arguments() {
    let (_dir, file) = pure_project();
    let result = run(
        &file,
        Some("shelf::books::greet"),
        &[json!("Ada")],
        RunMode::Run,
    );
    let message = result["diagnostics"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("typed run argument facts from Marrow"),
        "string args should be blocked: {result}"
    );
}

#[test]
fn run_does_not_touch_the_real_store() {
    // The project pins a native store and has no accepted catalog yet. A
    // presentation-only MCP run must not establish durable project state.
    let (dir, file) = project();
    let result = run(&file, Some("shelf::books::shout"), &[], RunMode::Run);
    assert_eq!(
        result["diagnostics"][0]["code"], "mcp.run.catalog",
        "{result}"
    );
    let store_file = dir.path().join("data").join("marrow.redb");
    assert!(
        !store_file.exists(),
        "a sandboxed run must never create or open the project store"
    );
    let catalog_file = dir.path().join("marrow.catalog.json");
    assert!(
        !catalog_file.exists(),
        "a presentation-only run must not write the accepted catalog"
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
            "canonical function-entry facts",
            "transitive effect facts",
            "durable-scope facts",
            "transaction facts",
            "runtime generation facts",
            "typed run protocol DTOs",
        ],
    );
}

#[test]
fn run_test_mode_blocks_args() {
    let (_dir, file) = pure_project();
    let result = run(&file, None, &[json!(1)], RunMode::Test);
    assert_eq!(
        result["diagnostics"][0]["code"], "mcp.run.args",
        "test mode args should be blocked before running tests: {result}"
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
        "root-only data helper",
        &[
            "catalog-bound saved-place identity",
            "typed children",
            "cursor/page facts",
            "snapshot/store generation",
            "stable data DTOs",
        ],
    );
    let integrity = data_integrity(&file, false);
    assert_eq!(integrity["dataAccess"], "disabled");
    assert_contract(
        &integrity,
        "presentation-only",
        "debug/admin advisory",
        &[
            "catalog/store identity",
            "store generation",
            "catalog epoch/digest",
            "typed repair or drift facts",
        ],
    );

    let children = data_children(
        Path::new("/nope/project/src/main.mw"),
        crate::data_explorer::DataChildrenRequest {
            segments: vec![crate::data_explorer::DataQuerySegmentDto::Root(
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
            "snapshot/store generation",
        ],
    );
}

#[test]
fn data_children_returns_paged_typed_segments_when_enabled() {
    let (_dir, file) = native_counter_project(1..=3);

    let result = data_children(
        &file,
        crate::data_explorer::DataChildrenRequest {
            segments: vec![crate::data_explorer::DataQuerySegmentDto::Root(
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
    assert_contract(
        &result,
        "presentation-only",
        "bounded typed data helper",
        &[
            "catalog-bound saved-place identity",
            "snapshot/store generation",
        ],
    );
}

#[test]
fn data_children_blocks_member_expansion_paths() {
    let (_dir, file) = native_counter_project(1..=1);

    let result = data_children(
        &file,
        crate::data_explorer::DataChildrenRequest {
            segments: vec![
                crate::data_explorer::DataQuerySegmentDto::Root("counter".into()),
                crate::data_explorer::DataQuerySegmentDto::Key(
                    crate::data_explorer::DataKeyDto::Int(1),
                ),
            ],
            limit: 1,
            cursor: None,
        },
        true,
    );

    assert_eq!(result["available"], false, "{result}");
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "member expansion should be blocked before any value reads: {result}"
    );
    assert_contract(
        &result,
        "presentation-only",
        "bounded typed data helper",
        &[
            "catalog-bound saved-place identity",
            "snapshot/store generation",
        ],
    );
}

#[test]
fn data_children_blocks_keyless_roots_before_member_scans() {
    let (_dir, file) = native_settings_project();

    let result = data_children(
        &file,
        crate::data_explorer::DataChildrenRequest {
            segments: vec![crate::data_explorer::DataQuerySegmentDto::Root(
                "settings".into(),
            )],
            limit: 1,
            cursor: None,
        },
        true,
    );

    assert_eq!(result["available"], false, "{result}");
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "keyless roots would require member scans and must stay blocked: {result}"
    );
    assert_contract(
        &result,
        "presentation-only",
        "bounded typed data helper",
        &[
            "catalog-bound saved-place identity",
            "snapshot/store generation",
        ],
    );
}
