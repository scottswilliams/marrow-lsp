//! Bind a project's accepted catalog and first-run lock the way every Marrow read
//! path does, so the editor checks source against the committed identity.
//!
//! The live native store is the sole write-time authority for accepted identity: a
//! valid stamped store wins unconditionally. With no store snapshot bound — the
//! fresh-checkout case of committed source plus a committed `marrow.lock` and no
//! store yet — the committed lock seeds first-run adoption, so the checked program
//! re-establishes the lock's committed ids instead of minting fresh random ones the
//! store could never address. A present-but-corrupt lock fails closed.
//!
//! Reading the full accepted catalog snapshot is on the editor's hot path (every
//! diagnostics debounce), so [`CatalogBindingCache`] keeps the bound snapshot and
//! re-reads it only when the store's committed identity advances. Identity is the
//! store's own monotonic commit fact — its UID plus the commit id and catalog epoch
//! of the latest commit — never a filesystem timestamp: redb commits write in place,
//! so two identity-advancing commits in the same coarse-mtime tick can share a file
//! `(mtime, len)`, and keying on that would serve a superseded snapshot. The commit
//! id is bumped by every commit, so a same-tick same-length write still invalidates.

use std::path::{Path, PathBuf};

use marrow_catalog::{CatalogLock, CatalogMetadata};
use marrow_check::{ProjectIoError, native_store_path, read_committed_lock};
use marrow_project::ProjectConfig;

use crate::store_view::{CommitMetadata, ReadOnlyStoreView, StoreUid};

/// The accepted catalog and the first-run lock to drive a source check: the store's
/// snapshot when a valid stamped store is present, otherwise the first-run `None`
/// accepted with the committed lock seeding adoption.
pub(crate) struct CatalogBinding {
    pub(crate) accepted: Option<CatalogMetadata>,
    pub(crate) lock: Option<CatalogLock>,
}

/// The committed lock to drive first-run adoption, read only when no accepted store
/// snapshot is present. A valid store is the sole accepted authority, so when one is
/// bound the lock is inert; on an empty store the committed lock seeds the fresh
/// checkout with its committed identity. A corrupt lock fails closed.
fn lock_for_adoption(
    root: &Path,
    accepted: Option<&CatalogMetadata>,
) -> Result<Option<CatalogLock>, ProjectIoError> {
    if accepted.is_some() {
        return Ok(None);
    }
    read_committed_lock(root)
}

/// Identifies a store's committed state across recomputes by its path and the store's
/// own monotonic commit fact — its UID and the commit id and catalog epoch of the
/// latest commit. `None` means the project has no native store on disk (memory backend,
/// or the file does not exist yet); the accepted catalog is then unconditionally the
/// first-run `None`, so no store open is needed and the cache key stays "absent".
///
/// This is filesystem-timestamp independent: every commit bumps `commit_id`, so two
/// identity-advancing commits that collapse to the same file `(mtime, len)` on a
/// coarse-mtime filesystem still produce different keys and force a re-bind. A
/// freshly-stamped store with a UID but no commit yet carries `None` for `commit_id`
/// and `catalog_epoch`, which still differs from the absent key and from any commit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoreCommitIdentity {
    path: PathBuf,
    store_uid: Option<StoreUid>,
    commit_id: Option<u64>,
    catalog_epoch: Option<u64>,
}

impl StoreCommitIdentity {
    fn from_commit(path: PathBuf, uid: Option<StoreUid>, commit: Option<CommitMetadata>) -> Self {
        Self {
            path,
            store_uid: uid,
            commit_id: commit.as_ref().map(|commit| commit.commit_id),
            catalog_epoch: commit.map(|commit| commit.catalog_epoch),
        }
    }
}

/// The outcome of probing the store against the cache for one recompute.
enum StoreProbe {
    /// The project has no native store on disk: a memory backend with no store path, or a
    /// native backend whose store file has not been created yet. A stable "absent" key whose
    /// accepted catalog is the first-run `None`, cached so a fresh checkout does not re-probe.
    Absent,
    /// A native store file is present but could not be opened or probed right now (a racing
    /// writer holds the redb lock, a permissions blip). The cache must not key on or serve
    /// identity it could not confirm, so this recompute neither updates the cache nor adopts a
    /// hit; the next recompute re-probes once the file is readable again.
    Unavailable,
    /// The store's commit identity matches the cache; the cached snapshot stands, and the
    /// expensive catalog-snapshot read was skipped. The identity is already the cache's key,
    /// so it carries nothing.
    Cached,
    /// The identity advanced past the cache (or the store newly appeared): the snapshot it
    /// now publishes, read under the same pin as the identity it is keyed on.
    Fresh {
        identity: StoreCommitIdentity,
        accepted: Option<CatalogMetadata>,
    },
}

/// Open the project's native store read-only and read its cheap commit identity from a
/// pinned snapshot, deciding under that same pin whether the cache is current. When the
/// identity matches `cached_key` the expensive catalog-snapshot read is skipped entirely;
/// only on a miss is the full accepted catalog read, and from the same pinned view the
/// identity was read under, so the bound snapshot and its key stay coherent against a
/// writer committing mid-probe.
///
/// The read-only handle lives only for this call, never across recomputes: redb takes an
/// OS lock for a handle's whole lifetime and a read-only handle excludes a writer, so a
/// cached open would block `marrow run`/`apply` from committing. Correctness — never
/// stale — does not require holding the handle; the cheap per-recompute open does.
fn probe_store(
    root: &Path,
    config: &ProjectConfig,
    cached_key: Option<&StoreCommitIdentity>,
) -> Result<StoreProbe, ProjectIoError> {
    let Some(path) = native_store_path(root, config)? else {
        return Ok(StoreProbe::Absent);
    };
    // A native project whose store file has not been created yet is the storeless first run,
    // not an unreadable store: its accepted catalog is unconditionally the first-run `None`, so
    // it caches as `Absent` rather than re-opening (and failing to open) the path every debounce.
    // Only a present file that will not open right now is `Unavailable`.
    if !path.exists() {
        return Ok(StoreProbe::Absent);
    }
    let Some(view) = ReadOnlyStoreView::open(&path) else {
        return Ok(StoreProbe::Unavailable);
    };
    let pin = view.pin()?;
    let uid = pin.store_uid()?;
    let commit = pin.commit_metadata()?;
    let identity = StoreCommitIdentity::from_commit(path, uid, commit);
    if cached_key == Some(&identity) {
        return Ok(StoreProbe::Cached);
    }
    let accepted = pin.accepted_catalog(root)?;
    Ok(StoreProbe::Fresh { identity, accepted })
}

/// Caches the bound accepted catalog keyed by the store's monotonic commit identity, so
/// the editor's per-debounce recompute reads the full catalog snapshot only when the
/// store's committed state actually advances. When the identity is unchanged the cached
/// snapshot is reused; when it advances — or the store appears or disappears — the next
/// bind re-reads, so the editor never shows identity from a superseded store commit, even
/// on a coarse-mtime filesystem where the commit left the file's `(mtime, len)` unchanged.
#[derive(Default)]
pub struct CatalogBindingCache {
    key: Option<StoreCommitIdentity>,
    accepted: Option<CatalogMetadata>,
    /// Whether [`Self::key`] reflects a real bind. A default cache and a cache whose
    /// last bind saw no store both hold `key: None`, but only the latter has bound;
    /// this flag distinguishes them so the first bind always reads the store.
    bound: bool,
    /// How many times the cache has re-bound the accepted snapshot (read the full
    /// catalog snapshot), for tests that assert an unchanged store is bound once across
    /// repeated recomputes and re-bound after the store's commit identity advances.
    #[cfg(test)]
    binds: usize,
}

impl CatalogBindingCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind the accepted catalog and first-run lock for a source check, reusing the
    /// cached accepted snapshot when the store's commit identity is unchanged since the
    /// last bind. The lock is always derived from the freshly bound `accepted`, so its
    /// read of `marrow.lock` is never cached: a committed-lock edit on a storeless project
    /// is reflected on the next recompute.
    pub fn bind(
        &mut self,
        root: &Path,
        config: &ProjectConfig,
    ) -> Result<CatalogBinding, ProjectIoError> {
        match probe_store(root, config, self.key.as_ref())? {
            StoreProbe::Cached => {}
            StoreProbe::Fresh { identity, accepted } => self.adopt(Some(identity), accepted),
            StoreProbe::Absent if self.bound && self.key.is_none() => {}
            StoreProbe::Absent => self.adopt(None, None),
            // A present-but-unconfirmable store neither updates nor is served as a hit:
            // bind the first-run `None` for this recompute without caching it, so the next
            // recompute re-probes rather than serving identity we could not read.
            StoreProbe::Unavailable => {
                return self.bind_uncached(root);
            }
        }
        self.current_binding(root)
    }

    /// Adopt a freshly bound accepted snapshot under its commit-identity key.
    fn adopt(&mut self, key: Option<StoreCommitIdentity>, accepted: Option<CatalogMetadata>) {
        self.key = key;
        self.accepted = accepted;
        self.bound = true;
        #[cfg(test)]
        {
            self.binds += 1;
        }
    }

    /// The binding for the currently cached accepted snapshot, with the first-run lock
    /// derived from it.
    fn current_binding(&self, root: &Path) -> Result<CatalogBinding, ProjectIoError> {
        let accepted = self.accepted.clone();
        let lock = lock_for_adoption(root, accepted.as_ref())?;
        Ok(CatalogBinding { accepted, lock })
    }

    /// The first-run binding for one recompute that could not confirm a present store,
    /// leaving the cache untouched so the next recompute re-probes. With no confirmed
    /// accepted snapshot the committed lock seeds adoption, exactly as a fresh checkout.
    fn bind_uncached(&self, root: &Path) -> Result<CatalogBinding, ProjectIoError> {
        let lock = lock_for_adoption(root, None)?;
        Ok(CatalogBinding {
            accepted: None,
            lock,
        })
    }

    #[cfg(test)]
    fn binds(&self) -> usize {
        self.binds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marrow_store::tree::StoreUid;
    use marrow_store::{AccessMode, SealedStore};

    /// A native-store project with one keyed store, stamped: a real store carrying a
    /// committed catalog. Returns the root, config, and the store file path so a test can
    /// observe re-binds across store-file changes.
    fn stamped_native_project() -> (tempfile::TempDir, ProjectConfig, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
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

        let config = marrow_check::load_config(&root).unwrap();
        let program = marrow_check::check_project_against(&root, &config, None, None).unwrap();

        let data_dir = root.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let store_path = data_dir.join("marrow.redb");
        let store = SealedStore::open(&store_path, AccessMode::Create)
            .unwrap()
            .into_store();
        store
            .write_store_uid(&StoreUid::new("store_00000000000000000000000000000001").unwrap())
            .unwrap();
        marrow_run::evolution::commit_catalog_baseline(&store, &program).unwrap();
        drop(store);

        (dir, config, store_path)
    }

    #[test]
    fn an_unchanged_store_binds_once_across_repeated_recomputes() {
        let (dir, config, _store_path) = stamped_native_project();
        let root = dir.path();
        let mut cache = CatalogBindingCache::new();

        let first = cache.bind(root, &config).unwrap();
        assert!(
            first.accepted.is_some(),
            "a stamped store binds an accepted catalog"
        );
        assert_eq!(cache.binds(), 1, "the first recompute reads the store");

        for _ in 0..5 {
            let again = cache.bind(root, &config).unwrap();
            assert_eq!(
                again.accepted, first.accepted,
                "an unchanged store binds the same accepted catalog"
            );
        }
        assert_eq!(
            cache.binds(),
            1,
            "repeated recomputes against an unchanged store reuse the cache instead of \
             re-reading the full catalog snapshot each time"
        );
    }

    #[test]
    fn a_new_commit_re_binds_so_the_editor_never_shows_stale_identity() {
        let (dir, config, store_path) = stamped_native_project();
        let root = dir.path();
        let mut cache = CatalogBindingCache::new();

        cache.bind(root, &config).unwrap();
        assert_eq!(cache.binds(), 1);
        cache.bind(root, &config).unwrap();
        assert_eq!(cache.binds(), 1, "an unchanged commit is not re-read");

        // A real commit advances the store's monotonic commit identity, so the next recompute
        // must re-bind rather than serve the snapshot the editor pinned before the commit.
        advance_commit(&store_path);

        cache.bind(root, &config).unwrap();
        assert_eq!(
            cache.binds(),
            2,
            "a store whose commit identity advanced must re-bind so the editor never shows \
             a superseded store commit"
        );
    }

    /// Read the store's current commit stamp, then commit the next dense `commit_id` over the
    /// same accepted catalog rows the way a managed run does: the catalog snapshot is unchanged,
    /// so the only thing that moves is the commit fact. Returns the byte length of the store file
    /// after the commit, so a caller can prove the commit left `(mtime, len)` unable to distinguish
    /// the two committed states.
    fn advance_commit(store_path: &Path) -> u64 {
        let store = SealedStore::open(store_path, AccessMode::Create)
            .unwrap()
            .into_store();
        let previous = store
            .read_commit_metadata()
            .unwrap()
            .expect("a stamped store carries a baseline commit");
        store
            .write_commit_metadata(&CommitMetadata {
                commit_id: previous.commit_id + 1,
                ..previous
            })
            .unwrap();
        drop(store);
        std::fs::metadata(store_path).unwrap().len()
    }

    #[test]
    fn a_same_length_commit_still_invalidates_the_cache() {
        let (dir, config, store_path) = stamped_native_project();
        let root = dir.path();
        let mut cache = CatalogBindingCache::new();

        cache.bind(root, &config).unwrap();
        assert_eq!(cache.binds(), 1, "the first recompute reads the store");
        cache.bind(root, &config).unwrap();
        assert_eq!(cache.binds(), 1, "an unchanged store is not re-read");

        // Two distinct committed states whose file `(mtime, len)` cannot tell them apart: redb
        // overwrites the fixed-width commit-metadata cell in place, so bumping `commit_id` over
        // the same catalog rows leaves the file's length unchanged, and a same-tick commit shares
        // its coarse mtime. The old `(mtime, len)` fingerprint key would treat the second state as
        // the first and serve the superseded snapshot; the commit-identity key advances with the
        // commit fact, so the cache re-binds.
        let len_before = std::fs::metadata(&store_path).unwrap().len();
        let len_after = advance_commit(&store_path);
        assert_eq!(
            len_before, len_after,
            "an in-place commit-id bump must leave the store file length unchanged, so this test \
             genuinely exercises a state the old (mtime, len) key could not distinguish"
        );

        cache.bind(root, &config).unwrap();
        assert_eq!(
            cache.binds(),
            2,
            "a commit that advanced the store's commit identity while leaving the file's \
             (mtime, len) unchanged must still re-bind, because the cache keys on the commit fact"
        );
    }

    #[test]
    fn a_storeless_project_binds_without_opening_a_store() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("marrow.json"),
            r#"{ "sourceRoots": ["src"], "store": { "backend": "memory" } }"#,
        )
        .unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("a.mw"),
            "module a\n\npub fn f(): int\n    return 1\n",
        )
        .unwrap();

        let config = marrow_check::load_config(root).unwrap();
        let mut cache = CatalogBindingCache::new();
        let binding = cache.bind(root, &config).unwrap();
        assert!(
            binding.accepted.is_none(),
            "a memory-backend project has no accepted store catalog"
        );
        // A memory backend has no store path, so the commit-identity key is permanently "absent":
        // the first recompute binds the storeless `None` and every later recompute is a cache hit.
        assert_eq!(cache.binds(), 1);
        cache.bind(root, &config).unwrap();
        assert_eq!(
            cache.binds(),
            1,
            "a storeless project must not re-bind on every recompute"
        );
    }

    /// A native-store project source tree whose store file has not been created yet. Returns
    /// the root, config, and the store path that does not yet exist on disk.
    fn native_project_without_store_file() -> (tempfile::TempDir, ProjectConfig, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
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
        let config = marrow_check::load_config(&root).unwrap();
        let store_path = root.join("data/marrow.redb");
        (dir, config, store_path)
    }

    #[test]
    fn a_native_project_without_a_store_file_caches_the_storeless_first_run() {
        let (dir, config, store_path) = native_project_without_store_file();
        let root = dir.path();
        assert!(
            !store_path.exists(),
            "this test exercises a native project whose store file has not been created yet"
        );

        let mut cache = CatalogBindingCache::new();
        let binding = cache.bind(root, &config).unwrap();
        assert!(
            binding.accepted.is_none(),
            "a native project with no store file yet has no accepted store catalog"
        );
        assert_eq!(
            cache.binds(),
            1,
            "the first recompute records the absent key"
        );
        cache.bind(root, &config).unwrap();
        assert_eq!(
            cache.binds(),
            1,
            "a missing store file is the cached storeless first run, not an unavailable store \
             re-probed on every recompute"
        );
    }

    #[test]
    fn a_store_locked_by_a_writer_binds_the_first_run_without_caching_then_re_binds() {
        let (dir, config, store_path) = stamped_native_project();
        let root = dir.path();
        let mut cache = CatalogBindingCache::new();

        let first = cache.bind(root, &config).unwrap();
        assert!(
            first.accepted.is_some(),
            "a stamped store binds its committed accepted catalog"
        );
        assert_eq!(cache.binds(), 1, "the first recompute reads the store");

        // A racing writer holding the redb file lock mid-debounce makes the read-only open fail
        // over a store file that genuinely exists, and advances the committed identity before it
        // releases. The locked recompute must accept the first-run `None` without poisoning the
        // cache: the writer's commit is the authority, so the cache must neither serve the first
        // bind's snapshot as confirmed nor stick on the `None` once the lock releases.
        let writer = SealedStore::open(&store_path, AccessMode::Create)
            .unwrap()
            .into_store();
        let previous = writer
            .read_commit_metadata()
            .unwrap()
            .expect("a stamped store carries a baseline commit");
        writer
            .write_commit_metadata(&CommitMetadata {
                commit_id: previous.commit_id + 1,
                ..previous
            })
            .unwrap();

        let blocked = cache.bind(root, &config).unwrap();
        assert!(
            blocked.accepted.is_none(),
            "an unconfirmable store binds the first-run None for this recompute"
        );
        assert_eq!(
            cache.binds(),
            1,
            "a present-but-locked store must not update or poison the cache"
        );
        assert_eq!(
            cache.accepted, first.accepted,
            "the cached accepted snapshot from before the lock is left untouched"
        );

        // The writer releases its lock; the next recompute re-probes, sees the advanced commit
        // identity, and re-binds the real committed catalog rather than serving the first-run
        // None it returned while the store was unreadable or the now-superseded first snapshot.
        drop(writer);
        let after = cache.bind(root, &config).unwrap();
        assert!(
            after.accepted.is_some(),
            "once the writer releases the lock the store rebinds its committed accepted catalog"
        );
        assert_eq!(
            cache.binds(),
            2,
            "the recompute after the lock releases re-reads and re-binds the advanced identity"
        );
    }
}
