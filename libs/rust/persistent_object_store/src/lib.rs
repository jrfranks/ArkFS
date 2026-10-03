//! PersistentObjectStore — content-addressed bytes.
//!
//! `put` returns success only after a local durable staging write, quorum
//! acknowledgments, and an atomic publish of the primary object name.
//!
//! # Mental model
//!
//! This crate does **not** know about files, directories, or FUSE. It stores
//! opaque blobs named by `blake3(blob)`. TemporalCore puts file bytes, encoded
//! attributes, and the encoded index all through the same `put`.
//!
//! Layout under the store root (FUSE `--data DIR` uses `DIR/primary`):
//!
//! ```text
//! <root>/objects/<64-hex>.obj    immutable payload
//! <root>/anchors/<name>          32-byte ObjectId pointer (today: temporal_index)
//! ```
//!
//! Staging file is a sibling `*.tmp`. Never `write` the final name in place.
//!
//! # Replication
//!
//! [`ReplicationBackend`] is the cross-node comms hook. The local write always
//! counts as 1 ack (`require_quorum`). Remotes are extra.
//!
//! - [`NoPeers`]: `replicate_*` → 0, `always_on_count` → 0, no replica dirs.
//!   Use [`open_isolated_store`] for single-node FUSE.
//! - [`LocalQuorum`]: writes extra directories (cluster *simulation*).
//! - [`open_local_quorum_store`]: first name is primary; remotes via [`LocalQuorum::remotes_under`].
//!
//! Onboarding: `docs/maintainer.md` (safe-write + isolation sections).

use arkfs_core::{ArkError, ObjectId, QuorumPolicy};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Report from a full integrity scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    pub objects_checked: u64,
    pub failures: Vec<String>,
}

impl IntegrityReport {
    /// True when the scan found no checksum failures.
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Backend for quorum replication.
///
/// Return values are **remote** acks only (not including local). [`NoPeers`]
/// returns `Ok(0)` and `always_on_count() == 0`.
///
/// Maintainer: implementations must be side-effect free on failure for the
/// primary path. Local write always counts as +1 in require_quorum.
/// See "Quorum and isolation" and "Safe-write contract" in maintainer.md.
pub trait ReplicationBackend: Send + Sync {
    /// Replicate object bytes to peers. Returns successful remote acks.
    ///
    /// Maintainer: must not create replica dirs for isolated FUSE (use NoPeers).
    /// Return 0 for no-peers backends. See open_isolated_store and NoPeers.
    fn replicate_object(&self, id: &ObjectId, data: &[u8]) -> Result<u32, ArkError>;

    /// Replicate a named root pointer (32-byte object id).
    ///
    /// Maintainer: same durability rules as replicate_object. Used for the
    /// temporal_index anchor.
    fn replicate_anchor(&self, name: &str, id_bytes: &[u8; 32]) -> Result<u32, ArkError>;

    /// Currently reachable remote replicas (not including the local primary).
    ///
    /// Maintainer: used in require_quorum to compute always_on + 1.
    /// For OwnerOnly single-node FUSE this must return 0.
    fn always_on_count(&self) -> u32;
}

/// Zero remote peers. Never creates `replicas/` and never iterates a peer list.
pub struct NoPeers;

impl ReplicationBackend for NoPeers {
    /// NoPeers: zero remote acks, no I/O.
    fn replicate_object(&self, _: &ObjectId, _: &[u8]) -> Result<u32, ArkError> {
        Ok(0)
    }

    /// NoPeers: zero remote acks, no I/O.
    fn replicate_anchor(&self, _: &str, _: &[u8; 32]) -> Result<u32, ArkError> {
        Ok(0)
    }

    /// NoPeers: no reachable remotes.
    fn always_on_count(&self) -> u32 {
        0
    }
}

/// Primary at `base/primary`. No replica directories, no peer I/O.
///
/// Maintainer: for single-node FUSE. Uses NoPeers so replicate_* returns 0
/// and always_on_count is 0. Enforces OwnerOnly semantics. See "OwnerOnly"
/// and "NoPeers" in maintainer.md. Must never create replicas/ dir.
pub fn open_isolated_store(base: impl AsRef<Path>) -> Result<PersistentObjectStore, ArkError> {
    PersistentObjectStore::open(base.as_ref().join("primary"), Box::new(NoPeers))
}

/// In-process replica directories simulating cluster peers of the primary.
///
/// Used by harness tests (`open_local_quorum_store` with several names).
/// `lose_node` / `restore_node` simulate partition without real networking.
/// `Clone` shares the lost-set so tests can partition after `open`.
#[derive(Clone)]
pub struct LocalQuorum {
    inner: Arc<LocalQuorumInner>,
}

struct LocalQuorumInner {
    replica_dirs: Vec<(String, PathBuf)>,
    lost: Mutex<HashSet<String>>,
}

impl LocalQuorum {
    /// Create replica objects/ and anchors/ dirs.
    pub fn new(replica_dirs: Vec<(String, PathBuf)>) -> Result<Self, ArkError> {
        for (_, p) in &replica_dirs {
            fs::create_dir_all(p.join("objects")).map_err(ArkError::from)?;
            fs::create_dir_all(p.join("anchors")).map_err(ArkError::from)?;
        }
        Ok(LocalQuorum {
            inner: Arc::new(LocalQuorumInner {
                replica_dirs,
                lost: Mutex::new(HashSet::new()),
            }),
        })
    }

    /// Remotes are `names[1..]` at `base/replicas/<name>`. `names[0]` is the
    /// primary label only. Empty `names` yields zero remotes.
    pub fn remotes_under(base: impl AsRef<Path>, names: &[&str]) -> Result<Self, ArkError> {
        let base = base.as_ref();
        let dirs = names
            .iter()
            .skip(1)
            .map(|name| (name.to_string(), base.join("replicas").join(name)))
            .collect();
        Self::new(dirs)
    }

    /// Simulate partition: later replicate_* skip this replica.
    pub fn lose_node(&self, node_id: &str) {
        self.inner.lost.lock().unwrap().insert(node_id.to_string());
    }

    /// Undo lose_node.
    pub fn restore_node(&self, node_id: &str) {
        self.inner.lost.lock().unwrap().remove(node_id);
    }

    /// Run `write` on every replica that is not lost; count successes.
    fn acks(&self, write: impl Fn(&Path) -> Result<(), ArkError>) -> u32 {
        let lost = self.inner.lost.lock().unwrap().clone();
        let mut acks = 0u32;
        for (name, dir) in &self.inner.replica_dirs {
            if lost.contains(name) {
                continue;
            }
            if write(dir).is_ok() {
                acks += 1;
            }
        }
        acks
    }
}

impl ReplicationBackend for LocalQuorum {
    /// Safe-write the object into each reachable replica directory.
    fn replicate_object(&self, id: &ObjectId, data: &[u8]) -> Result<u32, ArkError> {
        Ok(self.acks(|dir| {
            write_fsync(
                &dir.join("objects").join(format!("{}.obj", id.to_hex())),
                data,
            )
        }))
    }

    /// Safe-write the 32-byte anchor into each reachable replica.
    fn replicate_anchor(&self, name: &str, id_bytes: &[u8; 32]) -> Result<u32, ArkError> {
        Ok(self.acks(|dir| write_fsync(&dir.join("anchors").join(name), id_bytes)))
    }

    /// Replicas that are not currently marked lost.
    fn always_on_count(&self) -> u32 {
        let lost = self.inner.lost.lock().unwrap();
        self.inner
            .replica_dirs
            .iter()
            .filter(|(n, _)| !lost.contains(n))
            .count() as u32
    }
}

/// Safe-write content-addressed object store.
///
/// Maintainer: the only way to write durable objects or anchors. All writes
/// must go through put / set_anchor. Direct filesystem writes bypass safety.
/// See "Safe-write contract" and "put" in maintainer.md.
pub struct PersistentObjectStore {
    root: PathBuf,
    backend: Box<dyn ReplicationBackend>,
}

impl PersistentObjectStore {
    /// Create objects/ and anchors/ under `root`; take the replication backend.
    ///
    /// Maintainer: creates the directory tree. For single-node FUSE pass a
    /// NoPeers backend via open_isolated_store. Never pass a backend that
    /// creates replicas/ when using OwnerOnly. See open() and "QuorumPolicy".
    pub fn open(
        root: impl Into<PathBuf>,
        backend: Box<dyn ReplicationBackend>,
    ) -> Result<Self, ArkError> {
        let root = root.into();
        fs::create_dir_all(root.join("objects")).map_err(ArkError::from)?;
        fs::create_dir_all(root.join("anchors")).map_err(ArkError::from)?;
        Ok(PersistentObjectStore { root, backend })
    }

    /// Store bytes; **blocks until safe** (local fsync + quorum + publish).
    ///
    /// An intact existing primary is not rewritten. Catch-up still
    /// `replicate_object`s and still requires quorum. A checksum-mismatch
    /// primary is rewritten on the new-object path. Replicate or quorum
    /// failure discards new/rewrite staging and does not unpublish an intact
    /// existing name.
    ///
    /// Preconditions:
    /// - quorum policy is satisfiable for the backend (OwnerOnly always is).
    ///
    /// Postconditions on success:
    /// - object is durably staged (fsync), replicated to quorum, then published
    ///   via atomic rename + directory fsync.
    /// - returned ObjectId is the BLAKE3 of the bytes.
    ///
    /// Maintainer: safe-write contract: local fsync + quorum acks + publish.
    /// Never write final name in place. Staging is sibling .tmp.
    /// See "Safe-write contract", "put", "require_quorum".
    /// Conflict-before-write is enforced by caller (TemporalCore).
    pub fn put(&self, data: &[u8], quorum: QuorumPolicy) -> Result<ObjectId, ArkError> {
        let id = ObjectId::from_bytes(data);
        let final_path = self.object_path(&id);
        let staging = if final_path.exists() {
            match self.get(&id) {
                Ok(_) => None,
                Err(ArkError::Integrity { .. }) => {
                    let staging = sibling_tmp(&final_path);
                    write_fsync(&staging, data)?;
                    Some(staging)
                }
                Err(e) => return Err(e),
            }
        } else {
            let staging = sibling_tmp(&final_path);
            write_fsync(&staging, data)?;
            Some(staging)
        };
        let remote = self
            .backend
            .replicate_object(&id, data)
            .inspect_err(|_| discard_staging(staging.as_deref()))?;
        self.require_quorum(remote, quorum, staging.as_deref())?;
        if let Some(ref staging) = staging {
            publish(staging, &final_path)?;
        }
        Ok(id)
    }

    /// Read and re-hash. Checksum mismatch is [`ArkError::Integrity`], not silent.
    ///
    /// Maintainer: always re-hashes on read (fail closed on mismatch).
    /// Never trust on-disk data without verification. See "get" and
    /// verify_integrity.
    pub fn get(&self, id: &ObjectId) -> Result<Vec<u8>, ArkError> {
        let path = self.object_path(id);
        let data = match fs::read(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ArkError::not_found(id.to_hex()));
            }
            Err(e) => return Err(e.into()),
        };
        let actual = ObjectId::from_bytes(&data);
        if actual != *id {
            return Err(ArkError::integrity(format!(
                "{}: checksum mismatch (got {})",
                id.to_hex(),
                actual.to_hex()
            )));
        }
        Ok(data)
    }

    /// Store root (`objects/` and `anchors/` live under this).
    ///
    /// Maintainer: callers must not write under this root directly.
    /// All durable writes must go through put/set_anchor.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Count of published `*.obj` files and their total size in bytes.
    ///
    /// Staging `*.tmp` is ignored. Used by FUSE `statfs` (not a checksum scan).
    ///
    /// Maintainer: only counts objects/ entries ending in .obj. Does not
    /// walk anchors. See "usage" and statfs in fuse_facade.
    pub fn usage(&self) -> Result<(u64, u64), ArkError> {
        let dir = self.root.join("objects");
        let mut count = 0u64;
        let mut bytes = 0u64;
        if !dir.exists() {
            return Ok((0, 0));
        }
        for entry in fs::read_dir(&dir).map_err(ArkError::from)? {
            let entry = entry.map_err(ArkError::from)?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("obj") {
                continue;
            }
            count = count.saturating_add(1);
            bytes = bytes.saturating_add(entry.metadata().map_err(ArkError::from)?.len());
        }
        Ok((count, bytes))
    }

    /// Scan every `*.obj` under `objects/` and re-hash.
    ///
    /// Maintainer: used for integrity checks and tests. On mismatch returns
    /// Integrity error (fail closed). See "verify_integrity" and get().
    /// Does not touch anchors or staging files.
    pub fn verify_integrity(&self) -> Result<IntegrityReport, ArkError> {
        let dir = self.root.join("objects");
        let mut report = IntegrityReport {
            objects_checked: 0,
            failures: Vec::new(),
        };
        if !dir.exists() {
            return Ok(report);
        }
        for entry in fs::read_dir(&dir).map_err(ArkError::from)? {
            let entry = entry.map_err(ArkError::from)?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("obj") {
                continue;
            }
            report.objects_checked += 1;
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let expected = match ObjectId::from_hex(stem) {
                Ok(id) => id,
                Err(_) => {
                    report.failures.push(format!("{stem}: bad object id"));
                    continue;
                }
            };
            match fs::read(&path) {
                Ok(data) => {
                    let actual = ObjectId::from_bytes(&data);
                    if actual != expected {
                        report.failures.push(format!(
                            "{stem}: checksum mismatch (got {})",
                            actual.to_hex()
                        ));
                    }
                }
                Err(e) => report.failures.push(format!("{stem}: {e}")),
            }
        }
        Ok(report)
    }

    /// Atomically publish a named 32-byte root pointer (same safe-write as `put`).
    ///
    /// Maintainer: anchors are not content-addressed; they are mutable named
    /// pointers (currently only "temporal_index"). Same fsync + quorum +
    /// publish contract as put. See "Anchor" and "set_anchor".
    pub fn set_anchor(
        &self,
        name: &str,
        id: &ObjectId,
        quorum: QuorumPolicy,
    ) -> Result<(), ArkError> {
        validate_anchor_name(name)?;
        let final_path = self.anchor_path(name);
        let staging = sibling_tmp(&final_path);
        write_fsync(&staging, id.as_bytes())?;
        let remote = self
            .backend
            .replicate_anchor(name, id.as_bytes())
            .inspect_err(|_| discard_staging(Some(&staging)))?;
        self.require_quorum(remote, quorum, Some(&staging))?;
        publish(&staging, &final_path)?;
        Ok(())
    }

    /// `Ok(None)` if the name has never been set. Wrong length is Integrity.
    ///
    /// Maintainer: anchors are 32 raw bytes. Wrong size is Integrity (fail closed).
    /// Only "temporal_index" is used in Phase 0. See "Anchor".
    pub fn get_anchor(&self, name: &str) -> Result<Option<ObjectId>, ArkError> {
        validate_anchor_name(name)?;
        let path = self.anchor_path(name);
        match fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() != 32 {
                    return Err(ArkError::integrity(format!(
                        "anchor {name} has {} bytes",
                        bytes.len()
                    )));
                }
                let mut a = [0u8; 32];
                a.copy_from_slice(&bytes);
                Ok(Some(ObjectId(a)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// primary/objects/<hex>.obj
    pub fn object_path(&self, id: &ObjectId) -> PathBuf {
        self.root
            .join("objects")
            .join(format!("{}.obj", id.to_hex()))
    }

    /// primary/anchors/<name>
    pub fn anchor_path(&self, name: &str) -> PathBuf {
        self.root.join("anchors").join(name)
    }

    /// Local write counts as +1. On failure, discard `staging` (new names only;
    /// an existing primary is not unpublished).
    ///
    /// Maintainer: this enforces the quorum part of the safe-write contract.
    /// total = remote + 1. For OwnerOnly with NoPeers this is 1. On failure
    /// we discard staging (important for new objects). See "require_quorum",
    /// "QuorumPolicy", "Safe-write contract".
    fn require_quorum(
        &self,
        remote_acks: u32,
        quorum: QuorumPolicy,
        staging: Option<&Path>,
    ) -> Result<(), ArkError> {
        let total = remote_acks.saturating_add(1);
        let always_on = self.backend.always_on_count().saturating_add(1);
        if quorum.is_satisfied(total, always_on) {
            return Ok(());
        }
        discard_staging(staging);
        Err(ArkError::Quorum {
            got: total,
            need: quorum.required_acks(always_on),
        })
    }
}

/// Best-effort: unpublished staging must not remain as a leftover name.
///
/// Maintainer: on any replication or quorum failure we must remove the .tmp
/// so it does not become a stray durable file after crash. See "staging"
/// and "Safe-write contract".
fn discard_staging(staging: Option<&Path>) {
    if let Some(s) = staging {
        let _ = fs::remove_file(s);
    }
}

/// Anchors are `[A-Za-z0-9_]+` so they are safe as a single path component.
///
/// Maintainer: used to prevent path traversal or weird names in anchors/.
/// Only temporal_index is used today. See "Anchor" and set_anchor/get_anchor.
fn validate_anchor_name(name: &str) -> Result<(), ArkError> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(ArkError::invalid_argument("invalid anchor name"));
    }
    Ok(())
}

/// Staging name: `abc.obj` → `abc.obj.tmp` (unique per final filename).
///
/// Maintainer: per-final-filename tmp prevents collisions when multiple
/// objects are staged concurrently. Never use a shared tmp. See "staging".
fn sibling_tmp(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// `rename` staging → final, then `fsync` the parent directory (crash safety).
///
/// Maintainer: publish is the final step of safe-write. rename + fsync(dir)
/// ensures the object is visible and durable after crash. Must only be
/// called after quorum is satisfied. See "publish" and "safe-write".
fn publish(staging: &Path, final_path: &Path) -> Result<(), ArkError> {
    let parent = final_path
        .parent()
        .ok_or_else(|| ArkError::integrity("path has no parent"))?;
    fs::rename(staging, final_path).map_err(ArkError::from)?;
    fsync_dir(parent)
}

/// Write `path` via a sibling tmp, `sync_all`, rename, fsync parent.
///
/// Maintainer: this is the local durable staging step of safe-write.
/// Must fsync data file and then the parent dir after rename.
/// See "safe-write", "fsync", "staging".
fn write_fsync(path: &Path, data: &[u8]) -> Result<(), ArkError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(ArkError::from)?;
    }
    let tmp = sibling_tmp(path);
    {
        let mut f = File::create(&tmp).map_err(ArkError::from)?;
        f.write_all(data).map_err(ArkError::from)?;
        f.sync_all().map_err(ArkError::from)?;
    }
    fs::rename(&tmp, path).map_err(ArkError::from)?;
    if let Some(parent) = path.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

/// Directory fsync so a rename is durable. Open the dir as a file and `sync_all`.
///
/// Maintainer: required after rename of object or anchor so that the
/// directory entry is durable. Failure to fsync dir can lose the publish
/// across crash. See "fsync_dir" and safe-write.
fn fsync_dir(path: impl AsRef<Path>) -> Result<(), ArkError> {
    let f = File::open(path.as_ref()).map_err(ArkError::from)?;
    f.sync_all().map_err(ArkError::from)?;
    Ok(())
}

/// Primary at `base/primary`; remaining names are remote replica directories.
///
/// `replica_names[0]` is the primary label (not a remote). Remotes are
/// [`LocalQuorum::remotes_under`]. A single name therefore opens a store with
/// an **empty** [`LocalQuorum`] — implicit isolation, still a cluster backend.
///
/// Maintainer: for cluster simulation/harness tests. For real single-node
/// FUSE always use open_isolated_store + NoPeers. See "LocalQuorum".
pub fn open_local_quorum_store(
    base: impl AsRef<Path>,
    replica_names: &[&str],
) -> Result<PersistentObjectStore, ArkError> {
    if replica_names.is_empty() {
        return Err(ArkError::invalid_argument(
            "at least one replica name (the primary) is required",
        ));
    }
    let base = base.as_ref();
    let backend = LocalQuorum::remotes_under(base, replica_names)?;
    PersistentObjectStore::open(base.join("primary"), Box::new(backend))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use arkfs_test_review::{
        review_assert as assert, review_eq as assert_eq, review_ne as assert_ne,
    };

    /// put then get returns the same bytes.
    #[test]
    fn store_and_get_roundtrip() {
        let _g = arkfs_test_review::guard();
        let dir = tempfile::tempdir().unwrap();
        let store = open_local_quorum_store(dir.path(), &["n0", "n1", "n2"]).unwrap();
        let data = b"hello arkfs";
        let id = store.put(data, QuorumPolicy::n(2)).unwrap();
        assert_eq!(id, ObjectId::from_bytes(data));
        assert_eq!(store.get(&id).unwrap(), data);
        let report = store.verify_integrity().unwrap();
        assert!(report.ok(), "{:?}", report.failures);
    }

    /// CAS: two puts of the same bytes share ObjectId.
    #[test]
    fn identical_payloads_share_id() {
        let _g = arkfs_test_review::guard();
        let dir = tempfile::tempdir().unwrap();
        let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
        let a = store.put(b"same", QuorumPolicy::n(2)).unwrap();
        let b = store.put(b"same", QuorumPolicy::n(2)).unwrap();
        assert_eq!(a, b);
    }

    /// A planned harness flip, applied to the published object, fails get and the scan.
    #[test]
    fn bit_flip_detected_on_get_and_scan() {
        let _g = arkfs_test_review::guard();
        let dir = tempfile::tempdir().unwrap();
        let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
        let data = b"integrity-me";
        let id = store.put(data, QuorumPolicy::n(1)).unwrap();
        let mut chaos = simulation_harness::ChaosInjector::new();
        chaos.inject_bit_flip("n0", id.to_hex());
        let planned = chaos.take_bit_flips();
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].0, "n0");
        let path = store.object_path(&id);
        let mut buf = fs::read(&path).unwrap();
        simulation_harness::flip_bit_in_buffer(&mut buf, 0);
        fs::write(&path, &buf).unwrap();
        assert!(matches!(store.get(&id), Err(ArkError::Integrity { .. })));
        let report = store.verify_integrity().unwrap();
        assert!(!report.ok());
    }

    /// usage counts published `*.obj` bytes and ignores missing dirs.
    #[test]
    fn usage_counts_objects_not_tmp() {
        let _g = arkfs_test_review::guard();
        let dir = tempfile::tempdir().unwrap();
        let store = open_isolated_store(dir.path()).unwrap();
        assert_eq!(store.usage().unwrap(), (0, 0));
        let data = vec![7u8; 100];
        store.put(&data, QuorumPolicy::OwnerOnly).unwrap();
        let (count, bytes) = store.usage().unwrap();
        assert_eq!(count, 1);
        assert_eq!(bytes, 100);
        assert!(store.root().ends_with("primary"));
    }

    /// Staging path is a sibling .tmp of the object name.
    #[test]
    fn staging_names_are_per_filename() {
        let _g = arkfs_test_review::guard();
        let obj = PathBuf::from("/objects/abc.obj");
        let meta = PathBuf::from("/objects/abc.meta");
        assert_ne!(sibling_tmp(&obj), sibling_tmp(&meta));
        assert_eq!(sibling_tmp(&obj).file_name().unwrap(), "abc.obj.tmp");
    }

    /// set_anchor then get_anchor returns the same ObjectId.
    #[test]
    fn anchor_roundtrip() {
        let _g = arkfs_test_review::guard();
        let dir = tempfile::tempdir().unwrap();
        let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
        let id = store.put(b"idx", QuorumPolicy::n(1)).unwrap();
        store
            .set_anchor("temporal_index", &id, QuorumPolicy::n(1))
            .unwrap();
        assert_eq!(store.get_anchor("temporal_index").unwrap(), Some(id));
    }

    /// ENOSPC (errno 28) maps to ArkError::NoSpace, not a generic Io.
    #[test]
    fn enospc_is_no_space() {
        let _g = arkfs_test_review::guard();
        let e = std::io::Error::from_raw_os_error(28);
        assert!(matches!(ArkError::from(e), ArkError::NoSpace { .. }));
    }
}
