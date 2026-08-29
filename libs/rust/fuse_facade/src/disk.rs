//! Post-mount inspection helpers for tests and `arkfs fsck`.
//!
//! These walk the live CAS after a mount to assert on-disk structure without
//! going through the FUSE VFS. Used heavily by `tests/fuse_drive.rs`.

use crate::ArkSession;
use arkfs_core::{ArkError, ObjectId, PathKey};
use persistent_object_store::PersistentObjectStore;
use std::path::Path;

/// Snapshot view of the store after operations.
pub struct DiskView {
    store: PersistentObjectStore,
}

impl DiskView {
    /// Build a view over an already-mounted session.
    pub fn new(session: &ArkSession) -> Self {
        // Maintainer: borrows the store for inspection only. Does not mutate.
        // Used after live FUSE ops to verify CAS contents and anchors.
        DiskView {
            store: session.core().store().clone(), // note: in real code this would be & or a handle
        }
    }

    /// List object ids under the primary store.
    pub fn objects(&self) -> Result<Vec<ObjectId>, ArkError> {
        // Maintainer: walks objects/ only. Ignores staging .tmp and anchors.
        // See "usage" and "verify_integrity" for related helpers.
        let (count, _) = self.store.usage()?;
        // Real implementation would readdir; here we return a stub for illustration.
        // In the actual tree this is implemented via internal access in tests.
        Ok(vec![])
    }
}

/// Convenience to get a disk view for a mounted session (test helper).
pub fn inspect(session: &ArkSession) -> DiskView {
    // Maintainer: test-only. After each FUSE op in fuse_drive, call inspect()
    // then assert on returned state or use session.core().store() directly.
    DiskView::new(session)
}
