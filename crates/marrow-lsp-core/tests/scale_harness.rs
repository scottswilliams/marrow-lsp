//! Source-scale latency-budget harness.
//!
//! A reusable synthetic-project generator (`N` modules x `M` lines) plus the
//! interactive-request assertions that are the exit gate for interactive latency.
//!
//! The gate favors **scale-invariant work-count assertions** over wall-clock
//! numbers, because wall-clock budgets are flaky under CI load. The load-bearing
//! assertions here prove that a warm interactive request does bounded work that
//! does not grow with the project on disk:
//!
//! - the freshness check is an O(1) document-generation compare, so it survives the
//!   whole project being deleted from disk (it never re-reads closed files), and
//! - hover, navigation, semantic tokens, and diagnostics all answer from the cached
//!   analysis snapshot and the open buffer's shared `Arc` state, so they too survive
//!   the on-disk sources being deleted — there is no per-request disk walk.
//!
//! The wall-clock ceilings below are a deliberately generous backstop: seeded from
//! the measured warm baseline times a large safety factor, they catch only a
//! catastrophic regression (an accidental quadratic pass, or a reintroduced
//! per-request project walk), not a modest slowdown. They are not precise budgets.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lsp_types::Url;
use marrow_lsp_core::completion::completion;
use marrow_lsp_core::diagnostics::{path_to_url, snapshot_to_diagnostics};
use marrow_lsp_core::documents::Documents;
use marrow_lsp_core::hover::hover_with_index;
use marrow_lsp_core::navigation::{self, SnapshotIndices};
use marrow_lsp_core::semantic_tokens::semantic_tokens;
use marrow_lsp_core::workspace::{AnalysisSnapshot, BindingIndex, ReadSource, Workspace};

/// A generated project on disk: `modules` source files under `src/`, each about
/// `lines_per_module` lines, with the first module (`mod_0`) as the entry file the
/// harness drives requests against.
struct SyntheticProject {
    _dir: tempfile::TempDir,
    root: PathBuf,
    entry: PathBuf,
    entry_source: String,
}

impl SyntheticProject {
    fn entry_url(&self) -> Url {
        path_to_url(&self.entry).expect("entry path is a file URL")
    }
}

/// Write a synthetic project of `modules` files, each roughly `lines_per_module`
/// lines. Every module is self-contained and well-typed: a `base_<i>` function, a
/// `caller_<i>` that calls it (so navigation has a real definition/reference pair),
/// and filler functions padding the module to the requested size.
fn generate_project(modules: usize, lines_per_module: usize) -> SyntheticProject {
    assert!(modules >= 1, "a project needs at least one module");
    let dir = tempfile::tempdir().expect("create temp project dir");
    let root = dir.path().to_path_buf();
    fs::write(
        root.join("marrow.json"),
        r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
    )
    .expect("write marrow.json");
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src dir");

    let mut entry_source = String::new();
    for index in 0..modules {
        let source = module_source(index, lines_per_module);
        let path = src.join(format!("mod_{index}.mw"));
        fs::write(&path, &source).expect("write module file");
        if index == 0 {
            entry_source = source;
        }
    }

    SyntheticProject {
        _dir: dir,
        entry: src.join("mod_0.mw"),
        root,
        entry_source,
    }
}

/// One well-typed module of roughly `lines_per_module` lines.
fn module_source(index: usize, lines_per_module: usize) -> String {
    let mut source = format!(
        "module mod_{index}\n\n\
         pub fn base_{index}(): int\n    return {index}\n\n\
         pub fn caller_{index}(): int\n    return base_{index}()\n\n"
    );
    // Each filler function is three lines (signature, body, blank). Pad until the
    // module reaches the requested line count so the project is source-realistic.
    let mut filler = 0;
    while source.lines().count() < lines_per_module {
        source.push_str(&format!(
            "pub fn f_{index}_{filler}(): int\n    return {filler}\n\n"
        ));
        filler += 1;
    }
    source
}

/// Warm the analysis: open the entry file as a buffer, run one project recompute,
/// and build the navigation binding index (as the first navigation request would).
fn warm(project: &SyntheticProject) -> (Documents, Workspace) {
    let mut documents = Documents::new();
    documents.open(project.entry_url(), project.entry_source.clone());
    let mut workspace = Workspace::new();
    workspace
        .recompute(&project.entry, &documents)
        .expect("warm recompute succeeds for a clean project");
    // Building the binding index here mirrors the first navigation request; later
    // reads take the cached index without rebuilding or touching disk.
    workspace
        .choose_read_source(&project.entry)
        .expect("the entry file is analyzed");
    (documents, workspace)
}

/// The latest analysis snapshot and its cached binding index, as a warm read
/// handler pairs them. Never rebuilds the index or reads disk.
fn latest_facts<'a>(
    workspace: &'a mut Workspace,
    file: &Path,
) -> (&'a AnalysisSnapshot, &'a BindingIndex) {
    match workspace
        .choose_read_source(file)
        .expect("warm analysis answers for the entry file")
    {
        ReadSource::Latest => (
            workspace.latest().expect("latest snapshot present"),
            workspace
                .binding_index_cached()
                .expect("choose_read_source built the binding index"),
        ),
        ReadSource::LastGood => workspace
            .last_good_facts()
            .expect("last-good facts present"),
    }
}

/// The byte offset just after `needle`'s leading `prefix` — i.e. the start of the
/// identifier that follows `prefix` at `needle`'s first occurrence.
fn offset_after(source: &str, needle: &str, prefix: &str) -> usize {
    source.find(needle).expect("needle present in source") + prefix.len()
}

/// Build the navigation index resolver over the analyzed file set, exactly as the
/// LSP navigation handlers do: the open buffer's index where held, else one built
/// from the snapshot's own analyzed source (never from disk).
fn snapshot_indices<'a>(
    snapshot: &'a AnalysisSnapshot,
    documents: &'a marrow_lsp_core::documents::DocumentAnalysisSnapshot,
) -> SnapshotIndices<'a> {
    SnapshotIndices::new(
        snapshot
            .files
            .iter()
            .map(|file| (file.path.as_path(), file.source.as_str())),
        |path| documents.line_index_for_path(path),
    )
}

/// The freshness gate is an O(1) document-generation compare, not a filesystem
/// walk: it must keep reporting the warm snapshot as current even after every
/// source file (and the project config) is deleted from disk. A gate that re-read
/// closed files would flip or fail here.
#[test]
fn freshness_check_reads_no_disk_and_survives_deleting_the_project() {
    let project = generate_project(40, 48);
    let (documents, workspace) = warm(&project);

    assert!(
        workspace.latest_matches_sources(&documents),
        "the warm snapshot should match the editor-visible sources"
    );

    delete_all_sources(&project.root);

    assert!(
        workspace.latest_matches_sources(&documents),
        "freshness is a generation compare; deleting closed files from disk must not \
         change it, proving the fresh path performs zero closed-file disk reads"
    );
}

/// Every warm interactive request answers from the cached snapshot and the open
/// buffer's shared state, so all of them survive the on-disk project being deleted.
/// This is the scale-invariant core of the latency gate: a request that walked
/// the project per call would return `None` (or fail) once the files are gone.
#[test]
fn warm_requests_do_no_per_request_disk_work() {
    let project = generate_project(40, 48);
    let (documents, mut workspace) = warm(&project);
    let entry = project.entry.clone();
    let entry_url = project.entry_url();

    // A snapshot of the open buffer is a refcount bump, not a copy: the same text
    // allocation backs the document and the snapshot (the Arc-shared-snapshot
    // invariant, asserted here at project scale as the per-request document cost).
    let buffer = documents
        .snapshot(&entry_url)
        .expect("entry buffer is open");
    let document = documents.get(&entry_url).expect("entry buffer is open");
    assert!(
        std::ptr::eq(
            std::sync::Arc::as_ptr(&buffer.text),
            std::sync::Arc::as_ptr(&document.text),
        ),
        "a per-request document snapshot must share the buffer, not deep-clone it"
    );

    let def_offset = offset_after(&project.entry_source, "pub fn base_0", "pub fn ");
    let call_offset = offset_after(&project.entry_source, "return base_0()", "return ");

    // Diagnostics for the entry file, computed from the warm snapshot before deleting.
    let warm_diagnostic_count = entry_diagnostic_count(&workspace, &documents, &entry_url);

    delete_all_sources(&project.root);

    // Hover, definition, and references answer from the cached snapshot + index.
    let doc_analysis = documents.analysis_snapshot();
    {
        let (snapshot, index) = latest_facts(&mut workspace, &entry);
        assert!(
            hover_with_index(snapshot, index, &entry, def_offset).is_some(),
            "hover must answer from the cached snapshot after the disk sources are gone"
        );
        let indices = snapshot_indices(snapshot, &doc_analysis);
        assert!(
            navigation::definition(snapshot, index, &indices, &entry, call_offset).is_some(),
            "go-to-definition must resolve from the cached snapshot, not from disk"
        );
        let references =
            navigation::references(snapshot, index, &indices, &entry, def_offset, true)
                .expect("references resolves the symbol under the cursor");
        assert!(
            references.len() >= 2,
            "the definition and its one call site are both references, got {}",
            references.len()
        );
    }

    // Semantic tokens read the open buffer's cached lex/parse/index — no disk.
    let tokens = semantic_tokens(&buffer.lexed, &buffer.parsed, &buffer.index);
    assert!(
        !tokens.is_empty(),
        "semantic tokens must come from the cached buffer state after deletion"
    );

    // Completion reads the checked program and the open buffer's parse — no disk.
    let (program_snapshot, _) = latest_facts(&mut workspace, &entry);
    let _ = completion(
        &program_snapshot.program,
        &entry,
        buffer.text.as_ref(),
        &buffer.parsed,
        &buffer.lexed,
        call_offset,
    );

    // Diagnostics still map from the cached snapshot: the entry file's result is
    // unchanged by the disk deletion, proving the mapping is snapshot-served.
    let after = entry_diagnostic_count(&workspace, &documents, &entry_url);
    assert_eq!(
        after, warm_diagnostic_count,
        "diagnostics for an open file must be served from the snapshot, not re-read from disk"
    );
}

/// A generous wall-clock backstop over the full interactive request set on a
/// source-scale project. These ceilings are seeded from the measured warm baseline
/// times a large safety factor; they exist only to catch a catastrophic regression
/// (a quadratic pass or a reintroduced per-request project walk), not to police
/// small timing changes. The scale-invariant assertions above are the real gate.
#[test]
fn warm_request_latency_stays_under_a_generous_ceiling() {
    let project = generate_project(48, 48);
    let (documents, mut workspace) = warm(&project);
    let entry = project.entry.clone();
    let entry_url = project.entry_url();
    let buffer = documents
        .snapshot(&entry_url)
        .expect("entry buffer is open");

    let def_offset = offset_after(&project.entry_source, "pub fn base_0", "pub fn ");
    let call_offset = offset_after(&project.entry_source, "return base_0()", "return ");
    let doc_analysis = documents.analysis_snapshot();

    // Per-request ceilings: baseline warm p95 is single-digit to low-hundreds of ms;
    // 2 s leaves a ~100x safety margin so the gate never flakes under CI load.
    const REQUEST_CEILING: Duration = Duration::from_secs(2);

    let hover_time = timed(|| {
        let (snapshot, index) = latest_facts(&mut workspace, &entry);
        hover_with_index(snapshot, index, &entry, def_offset)
    });
    assert_within("hover", hover_time, REQUEST_CEILING);

    let nav_time = timed(|| {
        let (snapshot, index) = latest_facts(&mut workspace, &entry);
        let indices = snapshot_indices(snapshot, &doc_analysis);
        navigation::definition(snapshot, index, &indices, &entry, call_offset)
    });
    assert_within("definition", nav_time, REQUEST_CEILING);

    let refs_time = timed(|| {
        let (snapshot, index) = latest_facts(&mut workspace, &entry);
        let indices = snapshot_indices(snapshot, &doc_analysis);
        navigation::references(snapshot, index, &indices, &entry, def_offset, true)
    });
    assert_within("references", refs_time, REQUEST_CEILING);

    let tokens_time = timed(|| semantic_tokens(&buffer.lexed, &buffer.parsed, &buffer.index));
    assert_within("semanticTokens", tokens_time, REQUEST_CEILING);

    let completion_time = timed(|| {
        let (snapshot, _) = latest_facts(&mut workspace, &entry);
        completion(
            &snapshot.program,
            &entry,
            buffer.text.as_ref(),
            &buffer.parsed,
            &buffer.lexed,
            call_offset,
        )
    });
    assert_within("completion", completion_time, REQUEST_CEILING);

    // The diagnostics floor is a whole-project recheck, so its ceiling is larger.
    // This is the one budget that scales with project size today (incremental
    // reanalysis is future work); the generous ceiling still catches blow-ups.
    const RECHECK_CEILING: Duration = Duration::from_secs(10);
    let recheck_time = timed(|| {
        workspace
            .recompute(&entry, &documents)
            .expect("warm recheck succeeds");
    });
    assert_within(
        "diagnostics (project recheck)",
        recheck_time,
        RECHECK_CEILING,
    );
}

/// Run `f`, returning how long it took. The result is dropped; callers time the work,
/// not its output.
fn timed<T>(f: impl FnOnce() -> T) -> Duration {
    let start = Instant::now();
    let value = f();
    let elapsed = start.elapsed();
    drop(value);
    elapsed
}

fn assert_within(request: &str, elapsed: Duration, ceiling: Duration) {
    assert!(
        elapsed <= ceiling,
        "{request} took {elapsed:?}, over the generous {ceiling:?} ceiling — a warm request \
         this slow signals a per-request project walk or a quadratic pass"
    );
}

/// The number of diagnostics reported for the entry (open) file, mapped exactly as
/// the LSP diagnostics publish does.
fn entry_diagnostic_count(workspace: &Workspace, documents: &Documents, entry_url: &Url) -> usize {
    let snapshot = workspace.latest().expect("a snapshot has been computed");
    let urls: Vec<Url> = documents.urls().cloned().collect();
    let by_url = snapshot_to_diagnostics(snapshot, &urls, |url| {
        documents.get(url).map(|d| d.index.as_ref())
    });
    by_url.get(entry_url).map(Vec::len).unwrap_or(0)
}

/// Remove every source file and the project config from disk, leaving only the
/// in-memory analysis. A request that re-reads the project would now observe an
/// empty directory.
fn delete_all_sources(root: &Path) {
    let src = root.join("src");
    if src.exists() {
        fs::remove_dir_all(&src).expect("delete src dir");
    }
    let config = root.join("marrow.json");
    if config.exists() {
        fs::remove_file(&config).expect("delete marrow.json");
    }
}
