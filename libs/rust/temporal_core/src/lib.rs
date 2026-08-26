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
//! # Inode allocation
//!
//! `file_id == 0` means unassigned. Allocate whenever the new attrs have
//! `file_id == 0` (including recreate after a tombstone). Never persist 0.
//! Directory rename retargets every descendant path in one index persist.
//!
//! Onboarding: `docs/maintainer.md`.

mod index;

use arkfs_core::codec;
use arkfs_core::{
    ArkError, FileAttributes, FileType, ObjectId, PathKey, QuorumPolicy, SizePolicy, Timestamp,
};
use index::{decode_index, encode_index, DurableIndex, VersionRecord};
use persistent_object_store::PersistentObjectStore;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub use index::{
    decode_index as load_index, DurableIndex as PersistedIndex,
    PathHistory as PersistedPathHistory, VersionRecord as PersistedVersion,
};

/// Named store pointer to the latest encoded [`PersistedIndex`] object.
pub const INDEX_ANCHOR: &str = "temporal_index";

/// Linux `renameat2(RENAME_NOREPLACE)` — fail if the destination exists.
pub const RENAME_NOREPLACE: u32 = 1;
/// Linux `renameat2(RENAME_EXCHANGE)` — swap the two names (and their trees).
pub const RENAME_EXCHANGE: u32 = 2;

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
                ..Default::default()
            },
        };
        let clock = index.max_timestamp();
        Ok(TemporalCore {
            store,
            index: Mutex::new(index),
            commit: Mutex::new(()),
            clock: Mutex::new(clock),
            quorum,
        })
    }

    /// Latest hybrid-logical timestamp observed or produced by this core.
    pub fn now(&self) -> Timestamp {
        *self.clock.lock().unwrap()
    }

    /// Live-tree lookup. Tombstones are not found.
    pub fn lookup_current(&self, path: &str) -> Result<FileHandle, ArkError> {
        self.lookup(path, View::Live)
    }

    /// Historical lookup: last version with `at <= ts`. A later tombstone does not hide it.
    pub fn lookup_at_timestamp(&self, path: &str, ts: Timestamp) -> Result<FileHandle, ArkError> {
        self.lookup(path, View::AsOf(ts))
    }

    /// Lookup `path` in `view`. Parses the path first (`PathKey::parse`).
    pub fn lookup(&self, path: &str, view: View) -> Result<FileHandle, ArkError> {
        let path = PathKey::parse(path)?;
        self.lookup_key(&path, view)
    }

    /// Resolve FUSE inode → path → handle.
    ///
    /// `View::Live` uses the derived inode map (O(1)). `View::AsOf` still scans
    /// because historical names are not in that map.
    pub fn lookup_ino(&self, ino: u64, view: View) -> Result<FileHandle, ArkError> {
        let rec_path = {
            let index = self.index.lock().unwrap();
            let found = match view {
                View::Live => index.ino_to_path.get(&ino).cloned(),
                View::AsOf(_) => index.find_ino(ino, view),
            };
            found.ok_or_else(|| ArkError::not_found(format!("ino {ino}")))?
        };
        self.lookup_key(&rec_path, view)
    }

    /// Number of live (non-tombstone) paths. FUSE `statfs` `f_files`.
    pub fn live_path_count(&self) -> u64 {
        self.index.lock().unwrap().live_path_count()
    }

    /// Load file bytes from CAS. `handle.content_id` is the key, not the payload.
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
        if matches!(view, View::Live) {
            let kids = index.live_children.get(&dir).cloned().unwrap_or_default();
            return Ok(kids
                .into_iter()
                .map(|(name, ino, file_type)| DirEntry {
                    name,
                    ino,
                    file_type,
                })
                .collect());
        }
        let mut out = Vec::new();
        for (path, hist) in &index.paths {
            if let Some(name) = dir.immediate_child(path) {
                if let Some(rec) = hist.record_in_view(view) {
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

    /// Create `/` as inode 1 if missing, owned by uid/gid 0. Idempotent.
    pub fn ensure_root(&self) -> Result<FileHandle, ArkError> {
        self.ensure_root_as(0, 0)
    }

    /// Create `/` as inode 1 owned by `uid`/`gid` if missing. Idempotent.
    ///
    /// FUSE uses the mounting process ids so a non-root client can write `/`.
    pub fn ensure_root_as(&self, uid: u32, gid: u32) -> Result<FileHandle, ArkError> {
        if let Ok(h) = self.lookup_key(&PathKey::root(), View::Live) {
            return Ok(h);
        }
        let at = self.tick();
        let mut attrs = FileAttributes::new_dir(1, 0o755);
        attrs.uid = uid;
        attrs.gid = gid;
        self.commit_branch(BranchDelta {
            path: "/".into(),
            content: Some(Vec::new()),
            attrs: Some(attrs),
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
        let path = self.prepare_create(parent, name)?;
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
        let path = self.prepare_create(parent, name)?;
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
        let path = self.prepare_create(parent, name)?;
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

    /// Move `from` to `to_parent/to_name`. Directories take every descendant
    /// with them in a single index persist. `flags` are Linux `renameat2`
    /// bits ([`RENAME_NOREPLACE`], [`RENAME_EXCHANGE`]).
    pub fn rename(
        &self,
        from: &str,
        to_parent: &str,
        to_name: &str,
        flags: u32,
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
        if dest.is_under(&from) {
            return Err(ArkError::invalid_argument(
                "cannot rename into a descendant",
            ));
        }
        let exchange = flags & RENAME_EXCHANGE != 0;
        let noreplace = flags & RENAME_NOREPLACE != 0;
        if exchange && noreplace {
            return Err(ArkError::invalid_argument(
                "RENAME_EXCHANGE and RENAME_NOREPLACE are mutually exclusive",
            ));
        }
        if flags & !(RENAME_NOREPLACE | RENAME_EXCHANGE) != 0 {
            return Err(ArkError::invalid_argument("unsupported rename flags"));
        }
        if exchange {
            if from.is_under(&dest) {
                return Err(ArkError::invalid_argument(
                    "cannot exchange with a descendant",
                ));
            }
            let _existing = self.lookup_key(&dest, View::Live)?;
            self.exchange_tree(&from, &dest)?;
            return self.lookup_key(&dest, View::Live);
        }
        if let Ok(existing) = self.lookup_key(&dest, View::Live) {
            if noreplace {
                return Err(ArkError::already_exists(dest.as_str()));
            }
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
        }
        self.rename_tree(&from, &dest)?;
        self.lookup_key(&dest, View::Live)
    }

    /// Extra name for a live non-directory. Same `file_id` (hard link).
    /// Shares the source `content_id` / `attrs_id`; later writes fan out to
    /// every live name of that inode (see [`replace_content`]).
    pub fn link(&self, src: &str, to_parent: &str, to_name: &str) -> Result<FileHandle, ArkError> {
        let src = self.lookup(src, View::Live)?;
        if src.attrs.file_type == FileType::Directory {
            return Err(ArkError::is_a_directory(src.path.as_str()));
        }
        let dest = self.prepare_create(to_parent, to_name)?;
        let at = self.tick();
        let _commit = self.commit.lock().unwrap();
        let mut snapshot = self.index.lock().unwrap().clone();
        self.observe(at);
        if let Some(last) = snapshot.paths.get(&dest).and_then(|h| h.versions.last()) {
            if !last.tombstone {
                return Err(ArkError::already_exists(dest.as_str()));
            }
            if at <= last.at {
                return Err(ArkError::conflict("commit timestamp before latest version"));
            }
        }
        let src_rec = snapshot
            .paths
            .get(&src.path)
            .and_then(|h| h.versions.last())
            .cloned()
            .ok_or_else(|| ArkError::not_found(src.path.as_str()))?;
        if src_rec.tombstone {
            return Err(ArkError::not_found(src.path.as_str()));
        }
        let parent = snapshot.version_parent(&dest);
        snapshot.push(dest.clone(), src_rec.continue_as(at, parent, false));
        self.persist_index(snapshot)?;
        self.lookup_key(&dest, View::Live)
    }

    /// Create a node of any POSIX type (regular, fifo, socket, device).
    #[allow(clippy::too_many_arguments)]
    pub fn mknod(
        &self,
        parent: &str,
        name: &str,
        file_type: FileType,
        mode: u32,
        uid: u32,
        gid: u32,
        rdev: Option<u64>,
    ) -> Result<FileHandle, ArkError> {
        if file_type == FileType::Directory {
            return self.mkdir(parent, name, mode, uid, gid);
        }
        let path = self.prepare_create(parent, name)?;
        let mut attrs = if file_type == FileType::File {
            FileAttributes::new_file(0, mode & 0o7777)
        } else {
            let mut a = FileAttributes::new_file(0, mode & 0o7777);
            a.file_type = file_type;
            a.streams.clear();
            a.rdev = rdev;
            a
        };
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

    /// New content version of a regular file (FUSE fsync path).
    ///
    /// Every live hard-link name of the same `file_id` gets the new content in
    /// one index persist (POSIX inode semantics).
    pub fn replace_content(&self, path: &str, data: Vec<u8>) -> Result<FileHandle, ArkError> {
        let path = PathKey::parse(path)?;
        let h = self.lookup_key(&path, View::Live)?;
        if h.attrs.file_type == FileType::Directory {
            return Err(ArkError::is_a_directory(path.as_str()));
        }
        if h.attrs.file_type != FileType::File {
            return Err(ArkError::invalid_argument("not a regular file"));
        }
        let at = self.tick();
        self.commit_branch(BranchDelta {
            path: path.as_str().into(),
            content: Some(data),
            attrs: Some(h.attrs.clone()),
            at,
        })?;
        self.lookup_key(&path, View::Live)
    }

    /// Append one version. Public for tests and facades; prefer mkdir/create/unlink.
    ///
    /// After the primary path is written, the same content/attrs ids are pushed
    /// onto every other live name of `file_id` (hard-link fan-out) in this persist.
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

        if let Some(last) = snapshot.last(&path) {
            if at <= last.at {
                return Err(ArkError::conflict("commit timestamp before latest version"));
            }
        }

        let parent = snapshot.version_parent(&path);
        let last = snapshot.last(&path);
        let content = match content {
            Some(c) => c,
            None => match last {
                Some(l) => self.store.get(&l.content_id)?,
                None => Vec::new(),
            },
        };
        let mut attrs = match attrs {
            Some(a) => a,
            None => match last {
                Some(l) if !l.tombstone => codec::decode_attrs(&self.store.get(&l.attrs_id)?)?,
                _ => FileAttributes::default(),
            },
        };
        // Unassigned id (including create after tombstone) gets a new identity.
        // Hard link / rename pass a non-zero file_id and keep it.
        let mut next_file_id = snapshot.next_file_id.max(1);
        if attrs.file_id == 0 {
            attrs.file_id = next_file_id;
            next_file_id = attrs.file_id.saturating_add(1);
        } else if attrs.file_id >= next_file_id {
            next_file_id = attrs.file_id.saturating_add(1);
        }

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
        let siblings = snapshot.live_siblings(rec.file_id, &path);
        for p in &siblings {
            if let Some(last) = snapshot.last(p) {
                if at <= last.at {
                    return Err(ArkError::conflict("commit timestamp before latest version"));
                }
            }
        }
        let mut new_index = snapshot;
        new_index.next_file_id = next_file_id;
        new_index.push(path, rec.clone());
        for p in siblings {
            let parent = new_index.version_parent(&p);
            new_index.push(
                p,
                VersionRecord {
                    parent,
                    ..rec.clone()
                },
            );
        }
        self.persist_index(new_index)
    }

    /// Read-modify-write attrs at `path` (FUSE setattr / xattr). Reuses content.
    /// Fans out to every live hard-link name of the inode.
    pub fn commit_attrs(
        &self,
        path: &str,
        mutator: impl FnOnce(&mut FileAttributes),
        at: Timestamp,
    ) -> Result<(), ArkError> {
        let current = self.lookup(path, View::Live)?;
        let mut attrs = current.attrs.clone();
        mutator(&mut attrs);
        self.commit_branch(BranchDelta {
            path: current.path.as_str().into(),
            content: None,
            attrs: Some(attrs),
            at,
        })
    }

    /// [`Self::commit_attrs`] at the next clock tick. Returns the new live handle.
    pub fn commit_attrs_now(
        &self,
        path: &str,
        mutator: impl FnOnce(&mut FileAttributes),
    ) -> Result<FileHandle, ArkError> {
        let at = self.tick();
        self.commit_attrs(path, mutator, at)?;
        self.lookup(path, View::Live)
    }

    /// Path-key lookup: pick the version in `view`, hide tombstones, fill `nlink`.
    fn lookup_key(&self, path: &PathKey, view: View) -> Result<FileHandle, ArkError> {
        let rec = {
            let index = self.index.lock().unwrap();
            let hist = index
                .paths
                .get(path)
                .ok_or_else(|| ArkError::not_found(path.as_str()))?;
            let rec = hist
                .record_in_view(view)
                .cloned()
                .ok_or_else(|| ArkError::not_found(path.as_str()))?;
            if rec.tombstone {
                return Err(ArkError::not_found(path.as_str()));
            }
            rec
        };
        let mut h = self.materialize(path, &rec)?;
        h.attrs.nlink = self.link_count(path, &rec, view);
        Ok(h)
    }

    /// Directory: 2 + live subdirs. File: number of live paths with this `file_id`.
    fn link_count(&self, path: &PathKey, rec: &VersionRecord, view: View) -> u32 {
        let index = self.index.lock().unwrap();
        index.nlink(rec, path, view)
    }

    /// Swap two live names (and every descendant of each) in one persist.
    fn exchange_tree(&self, a: &PathKey, b: &PathKey) -> Result<(), ArkError> {
        let _commit = self.commit.lock().unwrap();
        let mut snapshot = self.index.lock().unwrap().clone();
        let at = self.tick();
        self.observe(at);

        let a_tree = snapshot.live_under(a);
        let b_tree = snapshot.live_under(b);
        if a_tree.is_empty() {
            return Err(ArkError::not_found(a.as_str()));
        }
        if b_tree.is_empty() {
            return Err(ArkError::not_found(b.as_str()));
        }

        let mut moves: Vec<(PathKey, VersionRecord)> = Vec::new();
        for src in &a_tree {
            let last = snapshot.last_live(src)?;
            let newp = src
                .rebase(a, b)
                .ok_or_else(|| ArkError::invalid_argument("exchange rebase"))?;
            moves.push((newp, last));
        }
        for src in &b_tree {
            let last = snapshot.last_live(src)?;
            let newp = src
                .rebase(b, a)
                .ok_or_else(|| ArkError::invalid_argument("exchange rebase"))?;
            moves.push((newp, last));
        }
        for src in a_tree.iter().chain(b_tree.iter()) {
            let last = snapshot.last_live(src)?;
            let parent = snapshot.version_parent(src);
            snapshot.push(src.clone(), last.continue_as(at, parent, true));
        }
        for (newp, last) in moves {
            let parent = snapshot.version_parent(&newp);
            snapshot.push(newp, last.continue_as(at, parent, false));
        }
        self.persist_index(snapshot)
    }

    /// Move `from` and every live descendant to `dest` in one persist.
    fn rename_tree(&self, from: &PathKey, dest: &PathKey) -> Result<(), ArkError> {
        let _commit = self.commit.lock().unwrap();
        let mut snapshot = self.index.lock().unwrap().clone();
        let at = self.tick();
        self.observe(at);

        let moving = snapshot.live_under(from);
        if moving.is_empty() {
            return Err(ArkError::not_found(from.as_str()));
        }

        if let Ok(last) = snapshot.last_live(dest) {
            let parent = snapshot.version_parent(dest);
            snapshot.push(dest.clone(), last.continue_as(at, parent, true));
        }

        for src in &moving {
            let last = snapshot.last_live(src)?;
            let newp = src
                .rebase(from, dest)
                .ok_or_else(|| ArkError::invalid_argument("rename rebase"))?;
            let dest_parent = snapshot.version_parent(&newp);
            snapshot.push(newp, last.continue_as(at, dest_parent, false));
            let src_parent = snapshot.version_parent(src);
            snapshot.push(src.clone(), last.continue_as(at, src_parent, true));
        }
        self.persist_index(snapshot)
    }

    /// Parent must be a live directory; dest name must not be live (tombstone is OK).
    fn prepare_create(&self, parent: &str, name: &str) -> Result<PathKey, ArkError> {
        let parent = PathKey::parse(parent)?;
        self.require_dir(&parent, View::Live)?;
        let path = parent.join(name)?;
        if self.lookup_key(&path, View::Live).is_ok() {
            return Err(ArkError::already_exists(path.as_str()));
        }
        Ok(path)
    }

    /// Live lookup that must be a directory (`ENOTDIR` otherwise).
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
            index.last_live(path)?
        };
        let at = self.tick();
        let _commit = self.commit.lock().unwrap();
        let mut snapshot = self.index.lock().unwrap().clone();
        self.observe(at);
        if let Some(cur) = snapshot.last(path) {
            if at <= cur.at {
                return Err(ArkError::conflict("commit timestamp before latest version"));
            }
        }
        let parent = snapshot.version_parent(path);
        snapshot.push(path.clone(), last.continue_as(at, parent, true));
        self.persist_index(snapshot)
    }

    /// `put` the encoded index then move the `temporal_index` anchor.
    fn persist_index(&self, mut new_index: DurableIndex) -> Result<(), ArkError> {
        new_index.rebuild_ino_map();
        let index_id = self.store.put(&encode_index(&new_index), self.quorum)?;
        self.store
            .set_anchor(INDEX_ANCHOR, &index_id, self.quorum)?;
        *self.index.lock().unwrap() = new_index;
        Ok(())
    }

    /// Decode attrs from CAS and build a handle. Does not compute `nlink`.
    fn materialize(&self, path: &PathKey, rec: &VersionRecord) -> Result<FileHandle, ArkError> {
        let attrs = codec::decode_attrs(&self.store.get(&rec.attrs_id)?)?;
        Ok(FileHandle {
            path: path.clone(),
            content_id: rec.content_id,
            attrs,
            committed_at: rec.at,
        })
    }

    /// Advance the hybrid-logical clock and return the new timestamp.
    fn tick(&self) -> Timestamp {
        let wall = wall_nanos();
        let mut c = self.clock.lock().unwrap();
        *c = c.tick(wall);
        *c
    }

    /// Merge an incoming timestamp into the local clock (never go backwards).
    fn observe(&self, at: Timestamp) {
        let mut c = self.clock.lock().unwrap();
        if at.logical > c.logical {
            *c = at;
        } else {
            c.wall_nanos = c.wall_nanos.max(at.wall_nanos);
        }
    }

    /// The CAS this core writes. FUSE tests use it via `inspect`, not this getter.
    pub fn store(&self) -> &PersistentObjectStore {
        &self.store
    }

    /// Cactus parent indexes for tests (`None` is the first version of the path).
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

/// Wall clock for hybrid timestamps. Missing system time becomes 0.
fn wall_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod engine_tests;
