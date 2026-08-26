//! TemporalCore — versioned path history with store-backed payloads.
//!
//! Durability is delegated exclusively to [`persistent_object_store::PersistentObjectStore`].
//! The in-memory index is a cache of the durable `temporal_index` anchor.
//!
//! # Cactus (never-delete)
//!
//! Each path has a list of [`PersistedVersion`] records. `unlink` / `rmdir`
//! append a **tombstone** version; they do not remove objects. [`View::Live`]
//! is the last version (hidden if tombstone). [`View::AsOf`] is the last
//! version with `at <= ts`.
//!
//! Directories are paths with `FileType::Directory`. Children are *not* stored
//! on the directory object; [`readdir`] scans the path map for immediate
//! children (`PathKey::immediate_child`).
//!
//! # Commit protocol
//!
//! 1. Take `commit` mutex (one writer).
//! 2. Clone the index snapshot; drop the index lock before I/O.
//! 3. Reject `at <= last.at` (**conflict-before-write**).
//! 4. `put` content, `put` encoded attrs, `put` new index, `set_anchor`.
//! 5. Install the new in-memory index.
//!
//! Do not hold `index` across `store.put`.
//!
//! # Inode allocation trap
//!
//! `file_id` is assigned when the **path key is new**. A tombstone still owns
//! the key, so unlink+create can publish `file_id = 0`. FUSE treats nodeid 0
//! as ENOENT. When changing this, treat a trailing tombstone as a new identity.
//!
//! Directory `rename` currently copies one path and tombstones the source; it
//! does **not** rewrite child paths. See `docs/maintainer.md` traps.
//!
//! Onboarding: `docs/maintainer.md`.

mod index;

use arkfs_core::codec;
use arkfs_core::{
    ArkError, FileAttributes, FileType, ObjectId, PathKey, QuorumPolicy, SizePolicy, Timestamp,
};
use index::{decode_index, encode_index, DurableIndex, PathHistory, VersionRecord};
use persistent_object_store::PersistentObjectStore;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub use index::{
    decode_index as load_index, DurableIndex as PersistedIndex,
    PathHistory as PersistedPathHistory, VersionRecord as PersistedVersion,
};

/// Named store pointer to the latest encoded [`PersistedIndex`] object.
pub const INDEX_ANCHOR: &str = "temporal_index";

/// Live tree vs a historical cut.
///
/// `AsOf` uses full [`Timestamp`] order. The FUSE CLI passes
/// `Timestamp::new(logical, u64::MAX)` so the entire logical tick is included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Live,
    AsOf(Timestamp),
}

/// Handle returned by lookup APIs (content is loaded via [`TemporalCore::read_content`]).
///
/// Cheap to clone. `content_id` is a CAS key, not the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHandle {
    pub path: PathKey,
    pub content_id: ObjectId,
    pub attrs: FileAttributes,
    pub committed_at: Timestamp,
}

/// Directory listing entry (`.` / `..` are the caller's job — FUSE adds them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub ino: u64,
    pub file_type: FileType,
}

/// Delta applied on a branch commit.
///
/// `content: None` means “reuse last content object”. `attrs: None` means
/// “decode last attrs”. Both None still creates a new version (touch).
#[derive(Debug, Clone)]
pub struct BranchDelta {
    pub path: String,
    pub content: Option<Vec<u8>>,
    pub attrs: Option<FileAttributes>,
    pub at: Timestamp,
}

/// Store-backed path history. Commits are serialized; lookups do not hold I/O locks.
///
/// `quorum` is forwarded to every `put` / `set_anchor`. FUSE uses `OwnerOnly`.
pub struct TemporalCore {
    store: PersistentObjectStore,
    index: Mutex<DurableIndex>,
    commit: Mutex<()>,
    clock: Mutex<Timestamp>,
    quorum: QuorumPolicy,
}

impl TemporalCore {
    /// Load `temporal_index` if present; otherwise start empty (`next_file_id = 1`).
    /// Call [`ensure_root`] before POSIX mkdir/create so `/` exists as inode 1.
    pub fn open(store: PersistentObjectStore, quorum: QuorumPolicy) -> Result<Self, ArkError> {
        let index = match store.get_anchor(INDEX_ANCHOR)? {
            Some(id) => decode_index(&store.get(&id)?)?,
            None => DurableIndex {
                next_file_id: 1,
                paths: Default::default(),
            },
        };
        let clock = max_timestamp(&index);
        Ok(TemporalCore {
            store,
            index: Mutex::new(index),
            commit: Mutex::new(()),
            clock: Mutex::new(clock),
            quorum,
        })
    }

    pub fn now(&self) -> Timestamp {
        *self.clock.lock().unwrap()
    }

    pub fn lookup_current(&self, path: &str) -> Result<FileHandle, ArkError> {
        self.lookup(path, View::Live)
    }

    pub fn lookup_at_timestamp(&self, path: &str, ts: Timestamp) -> Result<FileHandle, ArkError> {
        self.lookup(path, View::AsOf(ts))
    }

    pub fn lookup(&self, path: &str, view: View) -> Result<FileHandle, ArkError> {
        let path = PathKey::parse(path)?;
        self.lookup_key(&path, view)
    }

    /// Resolve FUSE inode → path → handle. Linear scan of the path map.
    pub fn lookup_ino(&self, ino: u64, view: View) -> Result<FileHandle, ArkError> {
        let rec_path = {
            let index = self.index.lock().unwrap();
            find_ino(&index, ino, view).ok_or_else(|| ArkError::not_found(format!("ino {ino}")))?
        };
        self.lookup_key(&rec_path, view)
    }

    pub fn read_content(&self, handle: &FileHandle) -> Result<Vec<u8>, ArkError> {
        self.store.get(&handle.content_id)
    }

    /// Immediate live (or as-of) children of `dir`, sorted by name. No `.` / `..`.
    pub fn readdir(&self, dir: &str, view: View) -> Result<Vec<DirEntry>, ArkError> {
        let dir = PathKey::parse(dir)?;
        let handle = self.lookup_key(&dir, view)?;
        if handle.attrs.file_type != FileType::Directory {
            return Err(ArkError::not_a_directory(dir.as_str()));
        }
        let index = self.index.lock().unwrap();
        let mut out = Vec::new();
        for (path, hist) in &index.paths {
            if let Some(name) = dir.immediate_child(path) {
                if let Some(rec) = record_in_view(hist, view) {
                    if !rec.tombstone {
                        out.push(DirEntry {
                            name: name.to_string(),
                            ino: rec.file_id,
                            file_type: rec.file_type,
                        });
                    }
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Create `/` as inode 1 if missing. Idempotent.
    pub fn ensure_root(&self) -> Result<FileHandle, ArkError> {
        if let Ok(h) = self.lookup_key(&PathKey::root(), View::Live) {
            return Ok(h);
        }
        let at = self.tick();
        self.commit_branch(BranchDelta {
            path: "/".into(),
            content: Some(Vec::new()),
            attrs: Some(FileAttributes::new_dir(1, 0o755)),
            at,
        })?;
        self.lookup_key(&PathKey::root(), View::Live)
    }

    /// Create a directory. `file_id` 0 is allocated inside `commit_branch`.
    pub fn mkdir(
        &self,
        parent: &str,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<FileHandle, ArkError> {
        let path = self.prepare_create(parent, name, true)?;
        let mut attrs = FileAttributes::new_dir(0, mode & 0o7777);
        attrs.uid = uid;
        attrs.gid = gid;
        let at = self.tick();
        self.commit_branch(BranchDelta {
            path: path.as_str().into(),
            content: Some(Vec::new()),
            attrs: Some(attrs),
            at,
        })?;
        self.lookup_key(&path, View::Live)
    }

    /// Create an empty regular file. Recreate after tombstone is allowed.
    pub fn create_file(
        &self,
        parent: &str,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<FileHandle, ArkError> {
        let path = self.prepare_create(parent, name, false)?;
        let mut attrs = FileAttributes::new_file(0, mode & 0o7777);
        attrs.uid = uid;
        attrs.gid = gid;
        let at = self.tick();
        self.commit_branch(BranchDelta {
            path: path.as_str().into(),
            content: Some(Vec::new()),
            attrs: Some(attrs),
            at,
        })?;
        self.lookup_key(&path, View::Live)
    }

    /// Symlink: target string is both `attrs.symlink_target` and content bytes.
    pub fn symlink(
        &self,
        parent: &str,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> Result<FileHandle, ArkError> {
        let path = self.prepare_create(parent, name, false)?;
        let mut attrs = FileAttributes::new_file(0, 0o777);
        attrs.file_type = FileType::Symlink;
        attrs.symlink_target = Some(target.into());
        attrs.uid = uid;
        attrs.gid = gid;
        attrs.streams.clear();
        let at = self.tick();
        self.commit_branch(BranchDelta {
            path: path.as_str().into(),
            content: Some(target.as_bytes().to_vec()),
            attrs: Some(attrs),
            at,
        })?;
        self.lookup_key(&path, View::Live)
    }

    /// Tombstone a non-directory. Historical `AsOf` lookups still see prior bytes.
    pub fn unlink(&self, path: &str) -> Result<(), ArkError> {
        let path = PathKey::parse(path)?;
        let h = self.lookup_key(&path, View::Live)?;
        if h.attrs.file_type == FileType::Directory {
            return Err(ArkError::is_a_directory(path.as_str()));
        }
        self.tombstone(&path)
    }

    /// Tombstone an empty directory. Root cannot be removed.
    pub fn rmdir(&self, path: &str) -> Result<(), ArkError> {
        let path = PathKey::parse(path)?;
        if path.is_root() {
            return Err(ArkError::invalid_argument("cannot remove root"));
        }
        let h = self.lookup_key(&path, View::Live)?;
        if h.attrs.file_type != FileType::Directory {
            return Err(ArkError::not_a_directory(path.as_str()));
        }
        if !self.readdir(path.as_str(), View::Live)?.is_empty() {
            return Err(ArkError::not_empty(path.as_str()));
        }
        self.tombstone(&path)
    }

    /// Copy one path to a new name and tombstone the source.
    ///
    /// Directory children are **not** rewritten (known limitation). Rejects
    /// rename into a descendant. Replacing a live dest tombstones dest first
    /// (empty-dir / type-match rules like POSIX).
    pub fn rename(
        &self,
        from: &str,
        to_parent: &str,
        to_name: &str,
    ) -> Result<FileHandle, ArkError> {
        let from = PathKey::parse(from)?;
        let src = self.lookup_key(&from, View::Live)?;
        let dest = {
            let parent = PathKey::parse(to_parent)?;
            self.require_dir(&parent, View::Live)?;
            parent.join(to_name)?
        };
        if dest == from {
            return Ok(src);
        }
        if dest.as_str().starts_with(&format!("{}/", from.as_str())) && !from.is_root() {
            return Err(ArkError::invalid_argument(
                "cannot rename into a descendant",
            ));
        }
        if let Ok(existing) = self.lookup_key(&dest, View::Live) {
            if existing.attrs.file_type == FileType::Directory {
                if src.attrs.file_type != FileType::Directory {
                    return Err(ArkError::is_a_directory(dest.as_str()));
                }
                if !self.readdir(dest.as_str(), View::Live)?.is_empty() {
                    return Err(ArkError::not_empty(dest.as_str()));
                }
            } else if src.attrs.file_type == FileType::Directory {
                return Err(ArkError::not_a_directory(dest.as_str()));
            }
            self.tombstone(&dest)?;
        }
        let content = self.store.get(&src.content_id)?;
        let at = self.tick();
        self.commit_branch(BranchDelta {
            path: dest.as_str().into(),
            content: Some(content),
            attrs: Some(src.attrs.clone()),
            at,
        })?;
        self.tombstone(&from)?;
        self.lookup_key(&dest, View::Live)
    }

    /// New content version of a regular file (FUSE fsync path).
    pub fn replace_content(&self, path: &str, data: Vec<u8>) -> Result<FileHandle, ArkError> {
        let path = PathKey::parse(path)?;
        let h = self.lookup_key(&path, View::Live)?;
        if h.attrs.file_type != FileType::File {
            return Err(ArkError::is_a_directory(path.as_str()));
        }
        let at = self.tick();
        self.commit_branch(BranchDelta {
            path: path.as_str().into(),
            content: Some(data),
            attrs: Some(h.attrs),
            at,
        })?;
        self.lookup_key(&path, View::Live)
    }

    /// Append one version. Public for tests and facades; prefer mkdir/create/unlink.
    pub fn commit_branch(&self, delta: BranchDelta) -> Result<(), ArkError> {
        let _commit = self.commit.lock().unwrap();
        let BranchDelta {
            path,
            content,
            attrs,
            at,
        } = delta;
        let path = PathKey::parse(path)?;
        let snapshot = self.index.lock().unwrap().clone();
        self.observe(at);

        if let Some(last) = snapshot.paths.get(&path).and_then(|h| h.versions.last()) {
            if at <= last.at {
                return Err(ArkError::conflict("commit timestamp before latest version"));
            }
        }

        let parent = snapshot.paths.get(&path).and_then(|h| {
            let n = h.versions.len();
            (n > 0).then_some((n - 1) as u32)
        });

        let (content, mut attrs, next_file_id) = if let Some(hist) = snapshot.paths.get(&path) {
            let last = hist
                .versions
                .last()
                .ok_or_else(|| ArkError::not_found(path.as_str()))?;
            let content = match content {
                Some(c) => c,
                None => self.store.get(&last.content_id)?,
            };
            let attrs = match attrs {
                Some(a) => a,
                None => codec::decode_attrs(&self.store.get(&last.attrs_id)?)?,
            };
            (content, attrs, snapshot.next_file_id)
        } else {
            let content = content.unwrap_or_default();
            let mut attrs = attrs.unwrap_or_default();
            let mut next = snapshot.next_file_id;
            if attrs.file_id == 0 {
                attrs.file_id = next;
                next += 1;
            } else if attrs.file_id >= next {
                next = attrs.file_id + 1;
            }
            (content, attrs, next)
        };

        attrs.set_logical_size(content.len() as u64, SizePolicy::Logical);
        attrs.touch_change(at.to_timespec());

        let content_id = self.store.put(&content, self.quorum)?;
        if let Some(s) = attrs.primary_stream_mut() {
            s.content_id = Some(*content_id.as_bytes());
        }
        let attrs_id = self.store.put(&codec::encode_attrs(&attrs), self.quorum)?;

        let rec = VersionRecord {
            content_id,
            attrs_id,
            at,
            parent,
            tombstone: false,
            file_id: attrs.file_id,
            file_type: attrs.file_type,
        };
        let mut new_index = snapshot;
        new_index.next_file_id = next_file_id;
        new_index.paths.entry(path).or_default().versions.push(rec);
        self.persist_index(new_index)
    }

    /// Read-modify-write attrs at `path` (FUSE setattr / xattr). Reuses content.
    pub fn commit_attrs(
        &self,
        path: &str,
        mutator: impl FnOnce(&mut FileAttributes),
        at: Timestamp,
    ) -> Result<(), ArkError> {
        let current = self.lookup(path, View::Live)?;
        let mut attrs = current.attrs;
        mutator(&mut attrs);
        self.commit_branch(BranchDelta {
            path: path.into(),
            content: None,
            attrs: Some(attrs),
            at,
        })
    }

    pub fn commit_attrs_now(
        &self,
        path: &str,
        mutator: impl FnOnce(&mut FileAttributes),
    ) -> Result<FileHandle, ArkError> {
        let at = self.tick();
        self.commit_attrs(path, mutator, at)?;
        self.lookup(path, View::Live)
    }

    fn lookup_key(&self, path: &PathKey, view: View) -> Result<FileHandle, ArkError> {
        let rec = {
            let index = self.index.lock().unwrap();
            let hist = index
                .paths
                .get(path)
                .ok_or_else(|| ArkError::not_found(path.as_str()))?;
            let rec = record_in_view(hist, view)
                .cloned()
                .ok_or_else(|| ArkError::not_found(path.as_str()))?;
            if rec.tombstone {
                return Err(ArkError::not_found(path.as_str()));
            }
            rec
        };
        self.materialize(path, &rec)
    }

    /// Parent must be a live directory; dest name must not be live (tombstone is OK).
    fn prepare_create(&self, parent: &str, name: &str, _dir: bool) -> Result<PathKey, ArkError> {
        let parent = PathKey::parse(parent)?;
        self.require_dir(&parent, View::Live)?;
        let path = parent.join(name)?;
        if self.lookup_key(&path, View::Live).is_ok() {
            return Err(ArkError::already_exists(path.as_str()));
        }
        Ok(path)
    }

    fn require_dir(&self, path: &PathKey, view: View) -> Result<FileHandle, ArkError> {
        let h = self.lookup_key(path, view)?;
        if h.attrs.file_type != FileType::Directory {
            return Err(ArkError::not_a_directory(path.as_str()));
        }
        Ok(h)
    }

    /// Append a tombstone that reuses the last content/attrs ids (no extra `put`).
    fn tombstone(&self, path: &PathKey) -> Result<(), ArkError> {
        let last = {
            let index = self.index.lock().unwrap();
            let hist = index
                .paths
                .get(path)
                .ok_or_else(|| ArkError::not_found(path.as_str()))?;
            hist.versions
                .last()
                .cloned()
                .ok_or_else(|| ArkError::not_found(path.as_str()))?
        };
        if last.tombstone {
            return Err(ArkError::not_found(path.as_str()));
        }
        let at = self.tick();
        let _commit = self.commit.lock().unwrap();
        let snapshot = self.index.lock().unwrap().clone();
        self.observe(at);
        if let Some(cur) = snapshot.paths.get(path).and_then(|h| h.versions.last()) {
            if at <= cur.at {
                return Err(ArkError::conflict("commit timestamp before latest version"));
            }
        }
        let parent = snapshot.paths.get(path).and_then(|h| {
            let n = h.versions.len();
            (n > 0).then_some((n - 1) as u32)
        });
        let rec = VersionRecord {
            content_id: last.content_id,
            attrs_id: last.attrs_id,
            at,
            parent,
            tombstone: true,
            file_id: last.file_id,
            file_type: last.file_type,
        };
        let mut new_index = snapshot;
        new_index
            .paths
            .entry(path.clone())
            .or_default()
            .versions
            .push(rec);
        self.persist_index(new_index)
    }

    /// `put` the encoded index then move the `temporal_index` anchor.
    fn persist_index(&self, new_index: DurableIndex) -> Result<(), ArkError> {
        let index_id = self.store.put(&encode_index(&new_index), self.quorum)?;
        self.store
            .set_anchor(INDEX_ANCHOR, &index_id, self.quorum)?;
        *self.index.lock().unwrap() = new_index;
        Ok(())
    }

    fn materialize(&self, path: &PathKey, rec: &VersionRecord) -> Result<FileHandle, ArkError> {
        let attrs = codec::decode_attrs(&self.store.get(&rec.attrs_id)?)?;
        Ok(FileHandle {
            path: path.clone(),
            content_id: rec.content_id,
            attrs,
            committed_at: rec.at,
        })
    }

    fn tick(&self) -> Timestamp {
        let wall = wall_nanos();
        let mut c = self.clock.lock().unwrap();
        *c = c.tick(wall);
        *c
    }

    fn observe(&self, at: Timestamp) {
        let mut c = self.clock.lock().unwrap();
        if at.logical > c.logical {
            *c = at;
        } else {
            c.wall_nanos = c.wall_nanos.max(at.wall_nanos);
        }
    }

    pub fn store(&self) -> &PersistentObjectStore {
        &self.store
    }

    #[cfg(test)]
    fn parents(&self, path: &str) -> Vec<Option<u32>> {
        let path = PathKey::parse(path).unwrap();
        self.index
            .lock()
            .unwrap()
            .paths
            .get(&path)
            .map(|h| h.versions.iter().map(|v| v.parent).collect())
            .unwrap_or_default()
    }
}

fn wall_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn max_timestamp(index: &DurableIndex) -> Timestamp {
    let mut max = Timestamp::ZERO;
    for hist in index.paths.values() {
        for v in &hist.versions {
            if v.at > max {
                max = v.at;
            }
        }
    }
    max
}

/// Live = last version (caller checks tombstone). AsOf = last `at <= ts`.
fn record_in_view(hist: &PathHistory, view: View) -> Option<&VersionRecord> {
    match view {
        View::Live => hist.versions.last(),
        View::AsOf(ts) => hist.versions.iter().rev().find(|v| v.at <= ts),
    }
}

fn find_ino(index: &DurableIndex, ino: u64, view: View) -> Option<PathKey> {
    for (path, hist) in &index.paths {
        if let Some(rec) = record_in_view(hist, view) {
            if !rec.tombstone && rec.file_id == ino {
                return Some(path.clone());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkfs_core::attr_map::{merge_from_fuse, FuseSetAttr};
    use arkfs_core::{DosFlags, MacOsFlags, NamedStream, Timespec};
    use persistent_object_store::open_local_quorum_store;

    fn core() -> (tempfile::TempDir, TemporalCore) {
        let dir = tempfile::tempdir().unwrap();
        let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
        (
            dir,
            TemporalCore::open(store, QuorumPolicy::Quorum(1)).unwrap(),
        )
    }

    #[test]
    fn commit_and_lookup_current() {
        let (_d, tc) = core();
        let t0 = Timestamp::new(1, 1000);
        tc.commit_branch(BranchDelta {
            path: "/notes.txt".into(),
            content: Some(b"v1".to_vec()),
            attrs: None,
            at: t0,
        })
        .unwrap();
        let h = tc.lookup_current("/notes.txt").unwrap();
        assert_eq!(tc.read_content(&h).unwrap(), b"v1");
        assert_eq!(h.attrs.logical_size, 2);
    }

    #[test]
    fn historical_lookup_never_delete() {
        let (_d, tc) = core();
        let t0 = Timestamp::new(1, 1000);
        let t1 = Timestamp::new(2, 2000);
        tc.commit_branch(BranchDelta {
            path: "/f".into(),
            content: Some(b"old".to_vec()),
            attrs: None,
            at: t0,
        })
        .unwrap();
        tc.commit_branch(BranchDelta {
            path: "/f".into(),
            content: Some(b"new".to_vec()),
            attrs: None,
            at: t1,
        })
        .unwrap();
        assert_eq!(
            tc.read_content(&tc.lookup_at_timestamp("/f", t0).unwrap())
                .unwrap(),
            b"old"
        );
        assert_eq!(
            tc.read_content(&tc.lookup_at_timestamp("/f", t1).unwrap())
                .unwrap(),
            b"new"
        );
        assert_eq!(
            tc.read_content(&tc.lookup_current("/f").unwrap()).unwrap(),
            b"new"
        );
        assert_eq!(tc.parents("/f"), vec![None, Some(0)]);
    }

    #[test]
    fn attr_historical_snapshot() {
        let (_d, tc) = core();
        let mut a0 = FileAttributes::new_file(0, 0o644);
        a0.dos.hidden = true;
        a0.dos.archive = true;
        a0.xattrs
            .insert("com.apple.quarantine".into(), b"q".to_vec());
        let t0 = Timestamp::new(1, 1);
        tc.commit_branch(BranchDelta {
            path: "/a".into(),
            content: Some(b"x".to_vec()),
            attrs: Some(a0.clone()),
            at: t0,
        })
        .unwrap();

        let t1 = Timestamp::new(2, 2);
        tc.commit_attrs(
            "/a",
            |attrs| {
                attrs.dos.hidden = false;
                attrs.mode = 0o600;
            },
            t1,
        )
        .unwrap();

        let past = tc.lookup_at_timestamp("/a", t0).unwrap();
        assert!(past.attrs.dos.hidden);
        assert!(past.attrs.dos.archive);
        assert_eq!(past.attrs.mode, 0o644);
        assert!(past.attrs.xattrs.contains_key("com.apple.quarantine"));

        let cur = tc.lookup_current("/a").unwrap();
        assert!(!cur.attrs.dos.hidden);
        assert!(cur.attrs.dos.archive);
        assert_eq!(cur.attrs.mode, 0o600);
        assert!(cur.attrs.xattrs.contains_key("com.apple.quarantine"));
    }

    #[test]
    fn fuse_partial_does_not_clobber_smb_macos() {
        let (_d, tc) = core();
        let mut a = FileAttributes::new_file(0, 0o644);
        a.dos = DosFlags {
            hidden: true,
            system: true,
            archive: true,
            ..Default::default()
        };
        a.macos = MacOsFlags {
            uf_immutable: true,
            ..Default::default()
        };
        a.dead_props.insert("{urn:ex}color".into(), "blue".into());
        a.streams.push(NamedStream {
            name: NamedStream::RESOURCE_FORK.into(),
            size: 8,
            content_id: None,
        });
        let t0 = Timestamp::new(1, 1);
        tc.commit_branch(BranchDelta {
            path: "/m".into(),
            content: Some(b"data".to_vec()),
            attrs: Some(a),
            at: t0,
        })
        .unwrap();

        let t1 = Timestamp::new(2, 2);
        tc.commit_attrs(
            "/m",
            |attrs| {
                merge_from_fuse(
                    attrs,
                    &FuseSetAttr {
                        mode: Some(0o700),
                        uid: Some(99),
                        gid: None,
                        size: None,
                        atime: None,
                        mtime: None,
                        ctime: None,
                    },
                    Timespec::new(2, 0),
                );
            },
            t1,
        )
        .unwrap();

        let h = tc.lookup_current("/m").unwrap();
        assert_eq!(h.attrs.mode, 0o700);
        assert_eq!(h.attrs.uid, 99);
        assert!(h.attrs.dos.hidden);
        assert!(h.attrs.dos.system);
        assert!(h.attrs.dos.archive);
        assert!(h.attrs.macos.uf_immutable);
        assert_eq!(
            h.attrs.dead_props.get("{urn:ex}color").map(String::as_str),
            Some("blue")
        );
        assert!(h
            .attrs
            .streams
            .iter()
            .any(|s| s.name == NamedStream::RESOURCE_FORK));
    }

    #[test]
    fn directory_roundtrip() {
        let (_d, tc) = core();
        let t0 = Timestamp::new(1, 1);
        tc.commit_branch(BranchDelta {
            path: "/dir".into(),
            content: Some(Vec::new()),
            attrs: Some(FileAttributes::new_dir(0, 0o755)),
            at: t0,
        })
        .unwrap();
        let h = tc.lookup_current("/dir").unwrap();
        assert_eq!(h.attrs.file_type, FileType::Directory);
        assert_eq!(h.attrs.nlink, 2);
        assert!(h.attrs.dos.directory);
        assert!(h.attrs.streams.is_empty());
        assert_eq!(h.attrs.mode, 0o755);
    }

    #[test]
    fn conflict_before_write_keeps_latest() {
        let (_d, tc) = core();
        tc.commit_branch(BranchDelta {
            path: "/c".into(),
            content: Some(b"new".to_vec()),
            attrs: None,
            at: Timestamp::new(2, 2),
        })
        .unwrap();
        let err = tc
            .commit_branch(BranchDelta {
                path: "/c".into(),
                content: Some(b"old".to_vec()),
                attrs: None,
                at: Timestamp::new(1, 1),
            })
            .unwrap_err();
        assert!(matches!(err, ArkError::Conflict { .. }));
        assert_eq!(
            tc.read_content(&tc.lookup_current("/c").unwrap()).unwrap(),
            b"new"
        );
    }

    #[test]
    fn index_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
            let tc = TemporalCore::open(store, QuorumPolicy::Quorum(1)).unwrap();
            tc.commit_branch(BranchDelta {
                path: "/p".into(),
                content: Some(b"persist".to_vec()),
                attrs: None,
                at: Timestamp::new(1, 1),
            })
            .unwrap();
            tc.commit_branch(BranchDelta {
                path: "/p".into(),
                content: Some(b"persist2".to_vec()),
                attrs: None,
                at: Timestamp::new(2, 2),
            })
            .unwrap();
        }
        let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
        let tc = TemporalCore::open(store, QuorumPolicy::Quorum(1)).unwrap();
        let h = tc.lookup_current("/p").unwrap();
        assert_eq!(tc.read_content(&h).unwrap(), b"persist2");
        assert_eq!(h.attrs.file_id, 1);
        assert_eq!(tc.parents("/p"), vec![None, Some(0)]);
    }

    #[test]
    fn mkdir_readdir_unlink_as_of() {
        let (_d, tc) = core();
        tc.ensure_root().unwrap();
        tc.mkdir("/", "d", 0o755, 0, 0).unwrap();
        tc.create_file("/d", "f.txt", 0o644, 0, 0).unwrap();
        let written = tc.replace_content("/d/f.txt", b"hello".to_vec()).unwrap();
        let names: Vec<_> = tc
            .readdir("/d", View::Live)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["f.txt"]);
        let before = written.committed_at;
        tc.unlink("/d/f.txt").unwrap();
        assert!(tc.lookup_current("/d/f.txt").is_err());
        assert!(tc.readdir("/d", View::Live).unwrap().is_empty());
        let past = tc.lookup_at_timestamp("/d/f.txt", before).unwrap();
        assert_eq!(tc.read_content(&past).unwrap(), b"hello");
        let ino_root = tc.lookup_current("/").unwrap().attrs.file_id;
        assert_eq!(ino_root, 1);
        let h = tc.lookup_ino(ino_root, View::Live).unwrap();
        assert!(h.path.is_root());
    }

    #[test]
    fn rename_and_rmdir() {
        let (_d, tc) = core();
        tc.ensure_root().unwrap();
        tc.mkdir("/", "a", 0o755, 0, 0).unwrap();
        tc.create_file("/a", "x", 0o644, 0, 0).unwrap();
        tc.replace_content("/a/x", b"z".to_vec()).unwrap();
        tc.rename("/a/x", "/a", "y").unwrap();
        assert!(tc.lookup_current("/a/x").is_err());
        assert_eq!(
            tc.read_content(&tc.lookup_current("/a/y").unwrap())
                .unwrap(),
            b"z"
        );
        tc.unlink("/a/y").unwrap();
        tc.rmdir("/a").unwrap();
        assert!(tc.lookup_current("/a").is_err());
    }

    #[test]
    fn create_rejects_existing() {
        let (_d, tc) = core();
        tc.ensure_root().unwrap();
        tc.create_file("/", "e", 0o644, 0, 0).unwrap();
        let err = tc.create_file("/", "e", 0o644, 0, 0).unwrap_err();
        assert!(matches!(err, ArkError::AlreadyExists { .. }));
    }
}
