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
///
/// Maintainer: commits are single-writer via `commit` mutex; never hold `index`
/// across store I/O (deadlock risk). See "Locks and concurrency" in maintainer.md.
pub struct TemporalCore {
    store: PersistentObjectStore,
    index: Mutex<DurableIndex>,
    commit: Mutex<()>,
    clock: Mutex<Timestamp>,
    quorum: QuorumPolicy,
}

impl TemporalCore {
    /// Load `temporal_index` if present; otherwise start empty (`next_file_id = 1`).
    ///
    /// Call [`ensure_root`] (or `ensure_root_as`) before any POSIX mkdir/create
    /// so that `/` exists as inode 1. The clock is seeded from the max timestamp
    /// found in the loaded index.
    ///
    /// Preconditions: store is a valid safe-write store.
    /// Postconditions: returned core owns the store and an in-memory snapshot
    /// of the durable index; all subsequent mutations go through `commit_branch`.
    ///
    /// Maintainer: `file_id == 0` means unassigned (Inode 0 trap). Never persist 0.
    /// See "Inode allocation" section and "Inode 0" trap in maintainer.md.
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
    ///
    /// Maintainer: never use for durability ordering; only for `--as-of` and
    /// conflict-before-write checks. See "Logical time" in maintainer.md.
    pub fn now(&self) -> Timestamp {
        *self.clock.lock().unwrap()
    }

    /// Live-tree lookup. Tombstones are not found.
    ///
    /// Maintainer: tombstones are never returned for Live; historical AsOf may
    /// still see prior content (never-delete). See "Never-delete in practice".
    pub fn lookup_current(&self, path: &str) -> Result<FileHandle, ArkError> {
        self.lookup(path, View::Live)
    }

    /// Historical lookup: last version with `at <= ts`. A later tombstone does not hide it.
    ///
    /// Maintainer: `View::AsOf` still scans (no inode map for historical names).
    /// See "View" and lookup_ino docs.
    pub fn lookup_at_timestamp(&self, path: &str, ts: Timestamp) -> Result<FileHandle, ArkError> {
        self.lookup(path, View::AsOf(ts))
    }

    /// Lookup `path` in `view`. Parses the path first (`PathKey::parse`).
    ///
    /// Maintainer: always go through `PathKey::parse`; never construct raw paths.
    /// See PathKey invariants in arkfs_core/src/id.rs.
    pub fn lookup(&self, path: &str, view: View) -> Result<FileHandle, ArkError> {
        let path = PathKey::parse(path)?;
        self.lookup_key(&path, view)
    }

    /// Resolve FUSE inode → path → handle.
    ///
    /// `View::Live` uses the derived inode map (O(1)). `View::AsOf` still scans
    /// because historical names are not in that map.
    ///
    /// Maintainer: root inode is always 1 (FUSE_ROOT_ID). Never return or accept
    /// inode 0 (Inode 0 trap).
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
    ///
    /// Maintainer: this is O(live paths) under the index lock; used for statfs.
    /// Never counts tombstones.
    pub fn live_path_count(&self) -> u64 {
        self.index.lock().unwrap().live_path_count()
    }

    /// Load file bytes from CAS. `handle.content_id` is the key, not the payload.
    ///
    /// Maintainer: content is immutable after publish. Reuses the same CAS object
    /// for hard links and historical versions.
    pub fn read_content(&self, handle: &FileHandle) -> Result<Vec<u8>, ArkError> {
        self.store.get(&handle.content_id)
    }

    /// Immediate live (or as-of) children of `dir`, sorted by name. No `.` / `..`.
    ///
    /// Directories do not store child pointers; this is a prefix scan of the
    /// path map using `PathKey::immediate_child`.
    ///
    /// Maintainer: readdir is a scan, not inode children. Adding a child does
    /// not rewrite the parent directory object. See "readdir" in maintainer.md.
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
    ///
    /// Maintainer: must allocate `file_id == 1` for root. See "Inode 0" trap
    /// and "ensure_root" guidance in maintainer.md.
    pub fn ensure_root(&self) -> Result<FileHandle, ArkError> {
        self.ensure_root_as(0, 0)
    }

    /// Create `/` as inode 1 owned by `uid`/`gid` if missing. Idempotent.
    ///
    /// FUSE uses the mounting process ids so a non-root client can write `/`.
    ///
    /// Maintainer: must assign fresh `file_id` when attrs still contain 0
    /// (Inode 0 trap). Never persist inode 0. Root is always inode 1.
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
    ///
    /// Parent must be a live directory with search+write permission for the
    /// caller. The name must not already exist as a live entry (tombstone OK).
    ///
    /// Maintainer: `file_id == 0` triggers allocation in commit_branch
    /// (Inode 0 trap). Directory rename retargets descendants (see rename_tree).
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
    ///
    /// Maintainer: `file_id == 0` is allocated in commit_branch (Inode 0 trap).
    /// Recreate after tombstone reuses the path key but gets a new version.
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
    ///
    /// Maintainer: symlink target is stored both in attrs and as content for
    /// readlink. Content bytes are the raw target (UTF-8 validated at FUSE layer).
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
    ///
    /// Maintainer: unlink appends a tombstone (never-delete). Bytes remain.
    /// Live lookup returns NotFound; AsOf before the tombstone still works.
    /// See "Never-delete in practice" and "Tombstone" in maintainer.md.
    /// Also see "Partial setattr" for any attr side effects.
    pub fn unlink(&self, path: &str) -> Result<(), ArkError> {
        let path = PathKey::parse(path)?;
        let h = self.lookup_key(&path, View::Live)?;
        if h.attrs.file_type == FileType::Directory {
            return Err(ArkError::is_a_directory(path.as_str()));
        }
        self.tombstone(&path)
    }

    /// Tombstone an empty directory. Root cannot be removed.
    ///
    /// Maintainer: directory must be empty at the time of the tombstone.
    /// rmdir is a tombstone, not a physical removal (never-delete).
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
    ///
    /// Maintainer: directory rename retargets every live descendant in one
    /// index persist (PathKey::rebase). See "Directory rename" trap.
    /// Open-file caches are resynced by inode in the FUSE layer.
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
    ///
    /// Maintainer: hard links share file_id. Open-file cache is per-inode
    /// (not per fh). See "Open-file cache is per-inode" trap. Writes via
    /// replace_content affect all names.
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
    ///
    /// Directories are delegated to mkdir. `file_id` allocation happens in
    /// commit_branch when 0 is seen.
    ///
    /// Maintainer: mknod for non-regular types still goes through commit_branch
    /// (Inode 0 allocation + conflict-before-write + safe publish).
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
    ///
    /// Maintainer: open-file cache is per-inode. release must persist before
    /// dropping fh; on failure keep buffer and return error.
    /// See "Open-file cache is per-inode" trap.
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
    ///
    /// Preconditions:
    /// - delta.at > last.at for the path (conflict-before-write checked here).
    /// - caller has already performed permission checks.
    ///
    /// Postconditions:
    /// - new version is durable (via store.put + set_anchor).
    /// - in-memory index is updated.
    /// - for hard links, every live sibling name also gets a new version record.
    ///
    /// Maintainer: conflict-before-write must reject before any put.
    /// Do not persist then roll back. See "Conflict-before-write" trap.
    /// Also: `file_id == 0` allocation here (Inode 0 trap).
    /// Directory rename fan-out is done via live_siblings + rebase in rename_tree.
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
    ///
    /// Maintainer: always use partial merge (attr_map::merge_from_* or
    /// commit_branch path). Never wholesale replace FileAttributes.
    /// See "Partial setattr" trap.
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
    ///
    /// Maintainer: uses the next logical tick. See tick/observe for clock rules.
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
    ///
    /// Maintainer: tombstones are hidden for both Live and AsOf. nlink is
    /// computed after materialization. See "Never-delete" and link_count.
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
    ///
    /// Maintainer: nlink is computed on the fly from the index (not stored in
    /// attrs). Hard links share file_id. See "nlink" logic in DurableIndex.
    fn link_count(&self, path: &PathKey, rec: &VersionRecord, view: View) -> u32 {
        let index = self.index.lock().unwrap();
        index.nlink(rec, path, view)
    }

    /// Swap two live names (and every descendant of each) in one persist.
    ///
    /// Maintainer: exchange is implemented by tombstoning old locations and
    /// re-creating at swapped paths using rebase. All in one persist_index.
    /// Directory rename/exchange retargets every live descendant (PathKey::rebase).
    /// See "Directory rename" trap.
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
    ///
    /// Maintainer: directory rename retargets every live descendant in one
    /// index persist via PathKey::rebase + tombstone old + create new.
    /// Open-file caches must be resynced by inode in the session layer.
    /// See "Directory rename" trap in maintainer.md.
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
    ///
    /// Maintainer: checks directory type and absence of live name. Used by
    /// mkdir/create_file/symlink/mknod/link. Permission checked by caller
    /// (FUSE layer or tests). See "prepare_create" call sites.
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
    ///
    /// Maintainer: used internally by prepare_create and rename. Callers
    /// must have already done X_OK permission check on the parent.
    fn require_dir(&self, path: &PathKey, view: View) -> Result<FileHandle, ArkError> {
        let h = self.lookup_key(path, view)?;
        if h.attrs.file_type != FileType::Directory {
            return Err(ArkError::not_a_directory(path.as_str()));
        }
        Ok(h)
    }

    /// Append a tombstone that reuses the last content/attrs ids (no extra `put`).
    ///
    /// Maintainer: tombstones are versions (never-delete). Conflict-before-write
    /// still applies. Used by unlink/rmdir. See "Tombstone" and
    /// "Conflict-before-write" traps.
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
    ///
    /// Maintainer: this is the only place the durable index anchor is advanced.
    /// Must be called while holding the logical commit lock. Safe-write is
    /// delegated to PersistentObjectStore::put + set_anchor.
    /// See "Safe-write contract" in maintainer.md.
    fn persist_index(&self, mut new_index: DurableIndex) -> Result<(), ArkError> {
        new_index.rebuild_ino_map();
        let index_id = self.store.put(&encode_index(&new_index), self.quorum)?;
        self.store
            .set_anchor(INDEX_ANCHOR, &index_id, self.quorum)?;
        *self.index.lock().unwrap() = new_index;
        Ok(())
    }

    /// Decode attrs from CAS and build a handle. Does not compute `nlink`.
    ///
    /// Maintainer: nlink is added by lookup_key after this. Content is not
    /// loaded here (lazy via read_content). See "materialize".
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
    ///
    /// Maintainer: monotonic; never goes backwards. See observe(). Logical time
    /// is the `--as-of` value. See "Logical time" in maintainer.md.
    fn tick(&self) -> Timestamp {
        let wall = wall_nanos();
        let mut c = self.clock.lock().unwrap();
        *c = c.tick(wall);
        *c
    }

    /// Merge an incoming timestamp into the local clock (never go backwards).
    ///
    /// Maintainer: used on every commit to keep clock moving forward.
    /// Conflict-before-write uses the resulting `at` values.
    fn observe(&self, at: Timestamp) {
        let mut c = self.clock.lock().unwrap();
        if at.logical > c.logical {
            *c = at;
        } else {
            c.wall_nanos = c.wall_nanos.max(at.wall_nanos);
        }
    }

    /// The CAS this core writes. FUSE tests use it via `inspect`, not this getter.
    ///
    /// Maintainer: direct access is for tests and the FUSE inspect path.
    /// Normal operation goes through commit_branch / persist_index.
    pub fn store(&self) -> &PersistentObjectStore {
        &self.store
    }

    /// Cactus parent indexes for tests (`None` is the first version of the path).
    ///
    /// Maintainer: test helper only. Parent indexes form the cactus structure.
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
///
/// Maintainer: used only by tick(). System time is combined with logical counter.
/// See hybrid logical timestamp rules.
fn wall_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod engine_tests;
