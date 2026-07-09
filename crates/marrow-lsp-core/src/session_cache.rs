//! Reuse a project's read-only saved-data session across data-view reads.
//!
//! Opening a saved-data session re-checks the project from disk and opens the native store.
//! The editor fires a data-view refresh on every diagnostics publish, so a naive open-per-refresh
//! storms both costs while the user types or a CLI commits in a loop. This cache keeps the
//! handle-free checked snapshot and the last roots read, keyed by the project's source-plus-store
//! generation, and reuses them while neither advanced.
//!
//! The cache never holds the store handle across reads: redb's read-only open takes a shared file
//! lock for the handle's whole lifetime, so a cached open would block `marrow run`/`apply` from
//! committing. It caches only the handle-free [`ProjectSurfaceSnapshot`] (the checked program) and
//! the last roots value; every read that must touch store bytes acquires a fresh short-lived
//! handle and drops it before returning.

use marrow_check::CheckedProgram;
use marrow_run::ProjectSurfaceSnapshot;

use crate::data_explorer::{SavedRootsResult, saved_roots};
use crate::store::{SavedDataSession, open_saved_data_snapshot};
use crate::store_view::StoreFileStat;
use crate::workspace::Project;

/// The project generation a cached session is valid for: the checked program's source digest
/// and its read-only-context digest — the accepted-catalog identity the source was checked
/// against. A source edit moves the first; a committed store change moves the second, so an
/// equal generation means neither source nor store advanced since the session was opened.
/// Compared by value; never rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionGeneration {
    source_digest: String,
    read_only_context_digest: String,
}

impl SessionGeneration {
    fn of(program: &CheckedProgram) -> Self {
        Self {
            source_digest: program.source_digest().to_string(),
            read_only_context_digest: program.read_only_context_digest().to_string(),
        }
    }
}

/// A cached saved-data session: the handle-free checked snapshot, the roots read from it, and the
/// store file's fingerprint at that read. The snapshot carries no store lock, so it survives
/// between reads; the roots serve the refresh storm without re-opening the store.
struct CachedSession {
    generation: SessionGeneration,
    store_stat: Option<StoreFileStat>,
    snapshot: ProjectSurfaceSnapshot,
    roots: SavedRootsResult,
}

/// Caches the checked saved-data snapshot and last roots read, keyed by the project's
/// source-plus-store generation and gated by the store file's `(mtime, len)` fingerprint, so the
/// editor's per-publish data-view refresh reuses them while neither source nor store advanced.
///
/// A generation change (source edit or committed store change the editor has re-checked) or a
/// store-file change (a CLI commit the editor has not yet re-checked) re-opens; an unchanged
/// project serves the cached roots without touching the store.
#[derive(Default)]
pub struct SavedDataSessionCache {
    cached: Option<CachedSession>,
    /// How many times the cache opened a read-only store handle, for the test that asserts
    /// repeated roots reads against an unchanged project open the store once and a source or
    /// store change re-opens.
    #[cfg(test)]
    store_opens: usize,
    /// How many times the cache re-checked the project from disk (opened a fresh snapshot), for
    /// the test that asserts a reused snapshot serves further reads without re-checking.
    #[cfg(test)]
    rechecks: usize,
}

impl SavedDataSessionCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The saved roots for the inspector, reusing the cached read while neither source nor store
    /// advanced. On a miss it opens the session once, reads the roots, caches them, and returns
    /// them; an unavailable or superseded project answers the typed unavailable roots.
    pub fn saved_roots(&mut self, project: &Project, program: &CheckedProgram) -> SavedRootsResult {
        let store_stat = native_store_stat(project);
        let generation = SessionGeneration::of(program);
        if self.is_current(&generation, &store_stat) {
            return self
                .cached
                .as_ref()
                .expect("current cache is present")
                .roots
                .clone();
        }
        match self.refresh(project, program, generation, store_stat) {
            Some(cached) => cached.roots.clone(),
            None => saved_roots(None),
        }
    }

    /// A live read-only session for a bounded saved-data read (children or one leaf), reusing the
    /// cached checked snapshot without re-checking when neither source nor store advanced. The
    /// returned session owns a fresh store handle the caller must drop promptly, so the store lock
    /// is never held across reads. `None` when the project is unavailable or the on-disk session
    /// no longer matches the editor-visible program.
    pub fn read_session(
        &mut self,
        project: &Project,
        program: &CheckedProgram,
    ) -> Option<SavedDataSession> {
        let store_stat = native_store_stat(project);
        let generation = SessionGeneration::of(program);
        if !self.is_current(&generation, &store_stat) {
            self.refresh(project, program, generation, store_stat)?;
        }
        let snapshot = &self.cached.as_ref()?.snapshot;
        let session = open_session(snapshot)?;
        #[cfg(test)]
        {
            self.store_opens += 1;
        }
        session_matching(session, program)
    }

    /// Whether the cache holds a snapshot for exactly this generation and store fingerprint. A
    /// `None` fingerprint never matches, so an unstattable store always re-opens.
    fn is_current(
        &self,
        generation: &SessionGeneration,
        store_stat: &Option<StoreFileStat>,
    ) -> bool {
        self.cached.as_ref().is_some_and(|cached| {
            &cached.generation == generation
                && store_stat.is_some()
                && &cached.store_stat == store_stat
        })
    }

    /// Re-check the project from disk, read its roots under a fresh handle, and cache them keyed by
    /// this generation and store fingerprint. The snapshot is cached only when its on-disk program
    /// matches the editor-visible program, so a cache hit always describes source the editor sees;
    /// a mismatch or an unavailable/locked store leaves the previous cache untouched and answers
    /// `None`. Returns the freshly cached session on success.
    fn refresh(
        &mut self,
        project: &Project,
        program: &CheckedProgram,
        generation: SessionGeneration,
        store_stat: Option<StoreFileStat>,
    ) -> Option<&CachedSession> {
        let snapshot = open_saved_data_snapshot(project)?;
        #[cfg(test)]
        {
            self.rechecks += 1;
        }
        let session = open_session(&snapshot)?;
        #[cfg(test)]
        {
            self.store_opens += 1;
        }
        let session = session_matching(session, program)?;
        let roots = saved_roots(Some(&session));
        self.cached = Some(CachedSession {
            generation,
            store_stat,
            snapshot,
            roots,
        });
        self.cached.as_ref()
    }

    #[cfg(test)]
    fn store_opens(&self) -> usize {
        self.store_opens
    }

    #[cfg(test)]
    fn rechecks(&self) -> usize {
        self.rechecks
    }
}

/// The native store file's cheap change fingerprint for this project, or `None` when the project
/// has no native store path (memory backend) or the file cannot be stat'd.
fn native_store_stat(project: &Project) -> Option<StoreFileStat> {
    let path = marrow_check::native_store_path(&project.root, &project.config).ok()??;
    StoreFileStat::read(&path)
}

/// Acquire a fresh read-only session from a cached snapshot. `None` when the store will not open
/// right now (a racing writer's lock, a permissions blip), a soft-unavailable state.
fn open_session(snapshot: &ProjectSurfaceSnapshot) -> Option<SavedDataSession> {
    snapshot
        .open_read_only()
        .ok()
        .map(SavedDataSession::from_read_session)
}

/// Keep a session only when its on-disk program matches the editor-visible program, so the data
/// it serves describes source the editor currently sees.
fn session_matching(
    session: SavedDataSession,
    program: &CheckedProgram,
) -> Option<SavedDataSession> {
    session.matches_checked_program(program).then_some(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::path::Path;

    use marrow_check::{
        check_project, check_project_against, project_store_lock, read_committed_lock,
    };
    use marrow_project::parse_config;
    use marrow_store::tree::{CommitMetadata, StoreUid};
    use marrow_store::{AccessMode, SealedStore};

    const CONFIG: &str =
        r#"{ "sourceRoots": ["src"], "store": { "backend": "native", "dataDir": "data" } }"#;

    /// A native-store project with one keyed store, checked and committed: a real inspectable
    /// store. Returns the temp dir and the resolved [`Project`].
    fn counter_project(value_type: &str) -> (tempfile::TempDir, Project) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("marrow.json"), CONFIG).unwrap();
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        write_source(root, value_type);

        let config = parse_config(CONFIG).unwrap();
        let (report, proposed) = check_project(root, &config).unwrap();
        assert!(!report.has_errors(), "{:#?}", report.diagnostics);
        let proposal = proposed.catalog.proposal.as_ref().unwrap();
        project_store_lock(root, proposal, &proposed.source_digest()).unwrap();
        let lock = read_committed_lock(root).unwrap();
        let program = check_project_against(root, &config, None, lock.as_ref()).unwrap();

        let data_dir = root.join("data");
        fs::create_dir_all(&data_dir).unwrap();
        let store = SealedStore::open(&data_dir.join("marrow.redb"), AccessMode::Create)
            .unwrap()
            .into_store();
        store
            .write_store_uid(&StoreUid::new("store_00000000000000000000000000000001").unwrap())
            .unwrap();
        marrow_run::evolution::commit_catalog_baseline(&store, &program).unwrap();
        drop(store);

        let project = Project {
            root: root.to_path_buf(),
            config,
        };
        (dir, project)
    }

    fn write_source(root: &Path, value_type: &str) {
        fs::write(
            root.join("src/app.mw"),
            format!(
                "\
module app

resource Counter
    required value: {value_type}

store ^counter(id: int): Counter
"
            ),
        )
        .unwrap();
    }

    /// The checked program the editor would hold for this project's on-disk source and store,
    /// so the cache's on-disk session matches it and caches.
    fn editor_program(project: &Project) -> CheckedProgram {
        open_saved_data_snapshot(project)
            .expect("native project opens a surface snapshot")
            .program()
            .clone()
    }

    /// Commit the next dense `commit_id` over the same catalog rows, the way a managed run does:
    /// the accepted catalog is unchanged, so only the store file's bytes and mtime move.
    fn advance_commit(project: &Project) {
        let store_path = project.root.join("data/marrow.redb");
        let store = SealedStore::open(&store_path, AccessMode::Create)
            .unwrap()
            .into_store();
        let previous = store
            .read_commit_metadata()
            .unwrap()
            .expect("a committed store carries a baseline commit");
        store
            .write_commit_metadata(&CommitMetadata {
                commit_id: previous.commit_id + 1,
                ..previous
            })
            .unwrap();
        drop(store);
    }

    #[test]
    fn repeated_roots_reads_against_an_unchanged_project_open_the_store_once() {
        let (_dir, project) = counter_project("int");
        let program = editor_program(&project);
        let mut cache = SavedDataSessionCache::new();

        let first = cache.saved_roots(&project, &program);
        assert!(first.available, "a committed store answers available roots");
        assert_eq!(cache.store_opens(), 1, "the first read opens the store");

        for _ in 0..5 {
            let again = cache.saved_roots(&project, &program);
            assert_eq!(
                again.roots, first.roots,
                "an unchanged project serves the same roots"
            );
        }
        assert_eq!(
            cache.store_opens(),
            1,
            "repeated roots reads against an unchanged source and store reuse the cached read \
             instead of storming read-only session opens"
        );
    }

    #[test]
    fn a_committed_store_change_re_opens_the_session() {
        let (_dir, project) = counter_project("int");
        let program = editor_program(&project);
        let mut cache = SavedDataSessionCache::new();

        cache.saved_roots(&project, &program);
        cache.saved_roots(&project, &program);
        assert_eq!(cache.store_opens(), 1, "an unchanged store is read once");

        advance_commit(&project);

        cache.saved_roots(&project, &program);
        assert_eq!(
            cache.store_opens(),
            2,
            "a committed store change moves the store fingerprint and must re-open the session"
        );
    }

    #[test]
    fn a_source_generation_change_does_not_serve_the_stale_cached_read() {
        let (_dir, project) = counter_project("int");
        let program = editor_program(&project);
        let mut cache = SavedDataSessionCache::new();

        let warm = cache.saved_roots(&project, &program);
        assert!(
            warm.available,
            "the committed store warms an available roots read"
        );

        // A saved schema-shape edit advances the editor-visible program's generation. Because it
        // no longer matches the committed store it reads as unavailable — the cache must re-evaluate
        // for the new generation, never serve the stale roots keyed to the old one.
        write_source(&project.root, "string");
        let edited = editor_program(&project);
        assert_ne!(
            SessionGeneration::of(&program),
            SessionGeneration::of(&edited),
            "a schema-shape edit moves the session generation"
        );

        let after = cache.saved_roots(&project, &edited);
        assert!(
            !after.available,
            "a source that drifted from the committed store answers unavailable, never the stale \
             cached roots keyed to the prior source generation"
        );
    }

    #[test]
    fn a_bounded_read_reuses_the_cached_snapshot_without_re_checking() {
        let (_dir, project) = counter_project("int");
        let program = editor_program(&project);
        let mut cache = SavedDataSessionCache::new();

        // Warm the cache with a roots read, then take several bounded read sessions.
        cache.saved_roots(&project, &program);
        assert_eq!(
            cache.rechecks(),
            1,
            "the first read re-checks the project from disk"
        );

        for _ in 0..5 {
            assert!(
                cache.read_session(&project, &program).is_some(),
                "a live read session is available for the unchanged project"
            );
        }
        assert_eq!(
            cache.rechecks(),
            1,
            "bounded reads against an unchanged project reuse the cached checked snapshot instead \
             of re-checking the project from disk each time"
        );
    }
}
