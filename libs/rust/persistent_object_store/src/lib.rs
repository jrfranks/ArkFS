//! PersistentObjectStore — content-addressed bytes.
//!
//! `put` returns success only after a local durable staging write, quorum
//! acknowledgments, and an atomic publish of the primary object name.

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
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Backend for quorum replication. Phase 0 ships [`LocalQuorum`].
pub trait ReplicationBackend: Send + Sync {
    /// Replicate object bytes to peers. Returns successful remote acks.
    fn replicate_object(&self, id: &ObjectId, data: &[u8]) -> Result<u32, ArkError>;

    /// Replicate a named root pointer (32-byte object id).
    fn replicate_anchor(&self, name: &str, id_bytes: &[u8; 32]) -> Result<u32, ArkError>;

    /// Currently reachable remote replicas (not including the local primary).
    fn always_on_count(&self) -> u32;
}

/// In-process replica directories simulating cluster peers of the primary.
pub struct LocalQuorum {
    replica_dirs: Vec<(String, PathBuf)>,
    lost: Mutex<HashSet<String>>,
}

impl LocalQuorum {
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

    pub fn lose_node(&self, node_id: &str) {
        self.lost.lock().unwrap().insert(node_id.to_string());
    }

    pub fn restore_node(&self, node_id: &str) {
        self.lost.lock().unwrap().remove(node_id);
    }

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
    fn replicate_object(&self, id: &ObjectId, data: &[u8]) -> Result<u32, ArkError> {
        Ok(self.acks(|dir| {
            write_fsync(
                &dir.join("objects").join(format!("{}.obj", id.to_hex())),
                data,
            )
        }))
    }

    fn replicate_anchor(&self, name: &str, id_bytes: &[u8; 32]) -> Result<u32, ArkError> {
        Ok(self.acks(|dir| write_fsync(&dir.join("anchors").join(name), id_bytes)))
    }

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
    pub fn put(&self, data: &[u8], quorum: QuorumPolicy) -> Result<ObjectId, ArkError> {
        let id = ObjectId::from_bytes(data);
        let final_path = self.object_path(&id);
        if final_path.exists() {
            self.get(&id)?;
            let _ = self.backend.replicate_object(&id, data);
            return Ok(id);
        }

        let staging = sibling_tmp(&final_path);
        write_fsync(&staging, data)?;
        let remote = self.backend.replicate_object(&id, data)?;
        self.require_quorum(remote, quorum, Some(&staging))?;
        publish(&staging, &final_path)?;
        Ok(id)
    }

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

    pub fn object_path(&self, id: &ObjectId) -> PathBuf {
        self.root
            .join("objects")
            .join(format!("{}.obj", id.to_hex()))
    }

    pub fn anchor_path(&self, name: &str) -> PathBuf {
        self.root.join("anchors").join(name)
    }

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

fn validate_anchor_name(name: &str) -> Result<(), ArkError> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(ArkError::invalid_argument("invalid anchor name"));
    }
    Ok(())
}

fn sibling_tmp(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

fn publish(staging: &Path, final_path: &Path) -> Result<(), ArkError> {
    let parent = final_path
        .parent()
        .ok_or_else(|| ArkError::integrity("path has no parent"))?;
    fs::rename(staging, final_path).map_err(ArkError::from)?;
    fsync_dir(parent)
}

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

fn fsync_dir(path: impl AsRef<Path>) -> Result<(), ArkError> {
    let f = File::open(path.as_ref()).map_err(ArkError::from)?;
    f.sync_all().map_err(ArkError::from)?;
    Ok(())
}

/// Primary at `base/primary`; remaining names are remote replica directories.
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

    #[test]
    fn store_and_get_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_local_quorum_store(dir.path(), &["n0", "n1", "n2"]).unwrap();
        let data = b"hello arkfs";
        let id = store.put(data, QuorumPolicy::Quorum(2)).unwrap();
        assert_eq!(id, ObjectId::from_bytes(data));
        assert_eq!(store.get(&id).unwrap(), data);
        let report = store.verify_integrity().unwrap();
        assert!(report.ok(), "{:?}", report.failures);
    }

    #[test]
    fn identical_payloads_share_id() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
        let a = store.put(b"same", QuorumPolicy::Quorum(2)).unwrap();
        let b = store.put(b"same", QuorumPolicy::Quorum(2)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn quorum_fails_closed_without_publishing() {
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

    #[test]
    fn bit_flip_detected_on_get_and_scan() {
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

    #[test]
    fn owner_only_succeeds_with_single_node() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_local_quorum_store(dir.path(), &["owner"]).unwrap();
        let id = store.put(b"solo", QuorumPolicy::OwnerOnly).unwrap();
        assert_eq!(store.get(&id).unwrap(), b"solo");
    }

    #[test]
    fn staging_names_are_per_filename() {
        let obj = PathBuf::from("/objects/abc.obj");
        let meta = PathBuf::from("/objects/abc.meta");
        assert_ne!(sibling_tmp(&obj), sibling_tmp(&meta));
        assert_eq!(sibling_tmp(&obj).file_name().unwrap(), "abc.obj.tmp");
    }

    #[test]
    fn anchor_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
        let id = store.put(b"idx", QuorumPolicy::Quorum(1)).unwrap();
        store
            .set_anchor("temporal_index", &id, QuorumPolicy::Quorum(1))
            .unwrap();
        assert_eq!(store.get_anchor("temporal_index").unwrap(), Some(id));
    }
}
