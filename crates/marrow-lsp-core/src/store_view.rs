//! The single read-only seam onto `marrow_store` for the editor core.
//!
//! Every production read of a project's native store flows through this module, so the
//! rest of the core never names a `marrow_store` type directly. A store-engine change —
//! a new physical key layout, a different snapshot type, the greenfield store redesign —
//! then lands against this one seam instead of every call site. The seam is read-only by
//! construction: it opens with [`AccessMode::Read`] and exposes only reads, so no editor
//! path can mutate saved data through it.
//!
//! A store that will not open right now (a racing writer holds the redb lock, a
//! permissions blip) is a soft "unavailable", surfaced as [`None`] from [`ReadOnlyStoreView::open`],
//! never an error shown to the user.

use std::path::Path;
use std::time::SystemTime;

use marrow_catalog::CatalogMetadata;
use marrow_check::{ProjectIoError, read_accepted_catalog_with_store_read_only};
use marrow_store::tree::ReadSnapshot;
use marrow_store::{AccessMode, SealedStore};

pub(crate) use marrow_store::tree::{CommitMetadata, StoreUid};

/// A cheap filesystem fingerprint of the native store file — its modification time and
/// length — used only to detect whether the store changed since a prior read so a cached
/// read can be reused without re-opening redb.
///
/// This is a change *detector*, never an identity *key*: an in-place commit that leaves the
/// file's `(mtime, len)` unchanged on a coarse-mtime filesystem is indistinguishable here,
/// which is why identity keys on the store's own commit fact and a lock-free store-epoch
/// fact is tracked upstream. On the fine-grained-mtime filesystems the editor runs against,
/// every commit moves the file's mtime, so an unchanged fingerprint is a sound "no store
/// write since the last read". A path that cannot be stat'd or reports no mtime yields
/// `None`, which never matches a recorded fingerprint, so the caller always re-opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoreFileStat {
    modified: SystemTime,
    len: u64,
}

impl StoreFileStat {
    /// Read the store file's fingerprint, or `None` when it cannot be stat'd or the platform
    /// does not report a modification time — in which case the caller must not skip the open.
    pub(crate) fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: metadata.modified().ok()?,
            len: metadata.len(),
        })
    }
}

/// A short-lived read-only handle onto a project's native store.
///
/// The handle lives only for a single probe, never across recomputes: redb takes an OS
/// lock for a handle's whole lifetime and a read-only handle excludes a writer, so a
/// cached open would block `marrow run`/`apply` from committing.
pub(crate) struct ReadOnlyStoreView {
    store: SealedStore,
}

impl ReadOnlyStoreView {
    /// Open the native store read-only. `None` when the file will not open right now — a
    /// soft-unavailable state (a racing writer's lock, a permissions blip), never an error.
    pub(crate) fn open(path: &Path) -> Option<Self> {
        SealedStore::open(path, AccessMode::Read)
            .ok()
            .map(|store| Self { store })
    }

    /// Pin a consistent read view of the store. The store's cheap commit identity and, on
    /// a cache miss, the full accepted catalog are read from this one pin, so the bound
    /// snapshot and the identity it is keyed on stay coherent against a writer committing
    /// mid-probe.
    pub(crate) fn pin(&self) -> Result<StorePin<'_>, ProjectIoError> {
        let snapshot = self.store.read_snapshot()?;
        Ok(StorePin {
            store: &self.store,
            _snapshot: snapshot,
        })
    }
}

/// A pinned read view of a store: every read taken from it observes one consistent
/// committed state. Holding the pin (`_snapshot`) is what makes the identity read and a
/// later accepted-catalog read describe the same commit.
pub(crate) struct StorePin<'view> {
    store: &'view SealedStore,
    _snapshot: ReadSnapshot<'view>,
}

impl StorePin<'_> {
    /// The store's stable UID, or `None` for a freshly-created store with no UID yet.
    pub(crate) fn store_uid(&self) -> Result<Option<StoreUid>, ProjectIoError> {
        Ok(self.store.read_store_uid()?)
    }

    /// The latest commit's identity fact (commit id + catalog epoch), or `None` for a
    /// stamped store with no commit yet.
    pub(crate) fn commit_metadata(&self) -> Result<Option<CommitMetadata>, ProjectIoError> {
        Ok(self.store.read_commit_metadata()?)
    }

    /// The full accepted catalog snapshot, read from this same pin so it is coherent with
    /// the identity already read. Reads through the canonical marrow bind-else-refuse path.
    pub(crate) fn accepted_catalog(
        &self,
        root: &Path,
    ) -> Result<Option<CatalogMetadata>, ProjectIoError> {
        read_accepted_catalog_with_store_read_only(root, Some(&**self.store))
    }
}
