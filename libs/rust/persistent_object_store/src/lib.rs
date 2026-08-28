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
//! - [`open_local_quorum_store`]: first name is primary, `skip(1)` are remotes.
//!
//! Onboarding: `docs/maintainer.md` (safe-write + isolation sections).

use arkfs_core::{ArkError, ObjectId, QuorumPolicy};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
pub trait ReplicationBackend: Send + Sync {
    /// Replicate object bytes to peers. Returns successful remote acks.
    fn replicate_object(&self, id: &ObjectId, data: &[u8]) -> Result<u32, ArkError>;

    /// Replicate a named root pointer (32-byte object id).
    fn replicate_anchor(&self, name: &str, id_bytes: &[u8; 32]) -> Result<u32, ArkError>;

    /// Currently reachable remote replicas (not including the local primary).
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
pub fn open_isolated_store(base: impl AsRef<Path>) -> Result<PersistentObjectStore, ArkError> {
    PersistentObjectStore::open(base.as_ref().join("primary"), Box::new(NoPeers))
}

/// In-process replica directories simulating cluster peers of the primary.
///
/// Used by harness tests (`open_local_quorum_store` with several names).
/// `lose_node` / `restore_node` simulate partition without real networking.
pub struct LocalQuorum {
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
            replica_dirs,
            lost: Mutex::new(HashSet::new()),
        })
    }

    /// Simulate partition: later replicate_* skip this replica.
    pub fn lose_node(&self, node_id: &str) {
        self.lost.lock().unwrap().insert(node_id.to_string());
    }

    /// Undo lose_node.
    pub fn restore_node(&self, node_id: &str) {
        self.lost.lock().unwrap().remove(node_id);
    }

    /// Run `write` on every replica that is not lost; count successes.
    fn acks(&self, write: impl Fn(&Path) -> Result<(), ArkError>) -> u32 {
        let lost = self.lost.lock().unwrap().clone();
        let mut acks = 0u32;
        for (name, dir) in &self.replica_dirs {
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
        let lost = self.lost.lock().unwrap();
        self.replica_dirs
            .iter()
            .filter(|(n, _)| !lost.contains(n))
            .count() as u32
    }
}

/// Safe-write content-addressed object store.
pub struct PersistentObjectStore {
    root: PathBuf,
    backend: Box<dyn ReplicationBackend>,
}

impl PersistentObjectStore {
    /// Create objects/ and anchors/ under `root`; take the replication backend.
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
    /// If the object already exists, still attempts `replicate_object` so a
    /// late-joining replica can catch up, then returns the id without rewriting
    /// the primary. Quorum failure on a *new* object deletes the staging file
    /// and does not publish.
    pub fn put(&self, data: &[u8], quorum: QuorumPolicy) -> Result<ObjectId, ArkError> {
        let id = ObjectId::from_bytes(data);
        let final_path = self.object_path(&id);
        if final_path.exists() {
            match self.get(&id) {
                Ok(_) => {
                    let _ = self.backend.replicate_object(&id, data);
                    return Ok(id);
                }
                Err(ArkError::Integrity { .. }) => {}
                Err(e) => return Err(e),
            }
        }

        let staging = sibling_tmp(&final_path);
        write_fsync(&staging, data)?;
        let remote = self.backend.replicate_object(&id, data)?;
        self.require_quorum(remote, quorum, Some(&staging))?;
        publish(&staging, &final_path)?;
        Ok(id)
    }

    /// Read and re-hash. Checksum mismatch is [`ArkError::Integrity`], not silent.
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
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Count of published `*.obj` files and their total size in bytes.
    ///
    /// Staging `*.tmp` is ignored. Used by FUSE `statfs` (not a checksum scan).
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
        let remote = self.backend.replicate_anchor(name, id.as_bytes())?;
        self.require_quorum(remote, quorum, Some(&staging))?;
        publish(&staging, &final_path)?;
        Ok(())
    }

    /// `Ok(None)` if the name has never been set. Wrong length is Integrity.
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

    /// Local write counts as +1. On failure, delete `staging` so the name is unpublished.
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
        if let Some(s) = staging {
            let _ = fs::remove_file(s);
        }
        Err(ArkError::Quorum {
            got: total,
            need: quorum.required_acks(always_on),
        })
    }
}

/// Anchors are `[A-Za-z0-9_]+` so they are safe as a single path component.
fn validate_anchor_name(name: &str) -> Result<(), ArkError> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(ArkError::invalid_argument("invalid anchor name"));
    }
    Ok(())
}

/// Staging name: `abc.obj` → `abc.obj.tmp` (unique per final filename).
fn sibling_tmp(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// `rename` staging → final, then `fsync` the parent directory (crash safety).
fn publish(staging: &Path, final_path: &Path) -> Result<(), ArkError> {
    let parent = final_path
        .parent()
        .ok_or_else(|| ArkError::integrity("path has no parent"))?;
    fs::rename(staging, final_path).map_err(ArkError::from)?;
    fsync_dir(parent)
}

/// Write `path` via a sibling tmp, `sync_all`, rename, fsync parent.
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
fn fsync_dir(path: impl AsRef<Path>) -> Result<(), ArkError> {
    let f = File::open(path.as_ref()).map_err(ArkError::from)?;
    f.sync_all().map_err(ArkError::from)?;
    Ok(())
}

/// Primary at `base/primary`; remaining names are remote replica directories.
///
/// `replica_names[0]` is the primary label (not a remote). `skip(1)` become
/// `base/replicas/<name>`. A single name therefore opens a store with an
/// **empty** [`LocalQuorum`] — implicit isolation, still a cluster backend.
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
    let primary = base.join("primary");
    let remotes: Vec<(String, PathBuf)> = replica_names
        .iter()
        .skip(1)
        .map(|name| (name.to_string(), base.join("replicas").join(name)))
        .collect();
    let backend = LocalQuorum::new(remotes)?;
    PersistentObjectStore::open(primary, Box::new(backend))
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
        let id = store.put(data, QuorumPolicy::Quorum(2)).unwrap();
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
        let a = store.put(b"same", QuorumPolicy::Quorum(2)).unwrap();
        let b = store.put(b"same", QuorumPolicy::Quorum(2)).unwrap();
        assert_eq!(a, b);
    }

    /// Quorum(2) with no remotes must not publish the object name.
    #[test]
    fn quorum_fails_closed_without_publishing() {
        let _g = arkfs_test_review::guard();
        let dir = tempfile::tempdir().unwrap();
        let backend = LocalQuorum::new(vec![
            ("n1".into(), dir.path().join("replicas/n1")),
            ("n2".into(), dir.path().join("replicas/n2")),
        ])
        .unwrap();
        backend.lose_node("n1");
        backend.lose_node("n2");
        let store =
            PersistentObjectStore::open(dir.path().join("primary"), Box::new(backend)).unwrap();
        let err = store.put(b"x", QuorumPolicy::Quorum(2)).unwrap_err();
        assert!(matches!(err, ArkError::Quorum { got: 1, need: 2 }));
        let id = ObjectId::from_bytes(b"x");
        assert!(!store.object_path(&id).exists());
        assert!(matches!(store.get(&id), Err(ArkError::NotFound { .. })));
    }

    /// Corrupting an object file makes get and integrity_scan fail.
    #[test]
    fn bit_flip_detected_on_get_and_scan() {
        let _g = arkfs_test_review::guard();
        let dir = tempfile::tempdir().unwrap();
        let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
        let data = b"integrity-me";
        let id = store.put(data, QuorumPolicy::Quorum(1)).unwrap();
        let path = store.object_path(&id);
        let mut buf = fs::read(&path).unwrap();
        buf[0] ^= 1;
        fs::write(&path, &buf).unwrap();
        assert!(matches!(store.get(&id), Err(ArkError::Integrity { .. })));
        let report = store.verify_integrity().unwrap();
        assert!(!report.ok());
    }

    /// OwnerOnly publishes with only the local write.
    #[test]
    fn owner_only_succeeds_with_single_node() {
        let _g = arkfs_test_review::guard();
        let dir = tempfile::tempdir().unwrap();
        let store = open_local_quorum_store(dir.path(), &["owner"]).unwrap();
        let id = store.put(b"solo", QuorumPolicy::OwnerOnly).unwrap();
        assert_eq!(store.get(&id).unwrap(), b"solo");
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

    /// NoPeers + OwnerOnly/AllAlwaysOn succeed; no replicas/ dir.
    #[test]
    fn isolated_owner_only_and_all_always_on() {
        let _g = arkfs_test_review::guard();
        let dir = tempfile::tempdir().unwrap();
        let store = open_isolated_store(dir.path()).unwrap();
        let id = store.put(b"solo", QuorumPolicy::OwnerOnly).unwrap();
        assert_eq!(store.get(&id).unwrap(), b"solo");
        store
            .set_anchor("temporal_index", &id, QuorumPolicy::AllAlwaysOn)
            .unwrap();
        assert!(!dir.path().join("replicas").exists());
        assert!(store
            .object_path(&id)
            .starts_with(dir.path().join("primary")));
    }

    /// NoPeers + Quorum(2) fails closed.
    #[test]
    fn isolated_quorum_two_fails_closed() {
        let _g = arkfs_test_review::guard();
        let dir = tempfile::tempdir().unwrap();
        let store = open_isolated_store(dir.path()).unwrap();
        let err = store.put(b"x", QuorumPolicy::Quorum(2)).unwrap_err();
        assert!(matches!(err, ArkError::Quorum { got: 1, need: 2 }));
        assert!(!store.object_path(&ObjectId::from_bytes(b"x")).exists());
        assert!(!dir.path().join("replicas").exists());
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
        let id = store.put(b"idx", QuorumPolicy::Quorum(1)).unwrap();
        store
            .set_anchor("temporal_index", &id, QuorumPolicy::Quorum(1))
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
