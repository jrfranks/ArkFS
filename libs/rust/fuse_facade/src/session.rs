//! In-process ArkFS node used by FUSE and by unit tests (no `/dev/fuse`).
//!
//! [`ArkSession`] owns a [`temporal_core::TemporalCore`]. Dirty file bytes are
//! shared per inode (not per fh). Root inode presented to FUSE is always
//! `FUSE_ROOT_ID` (1). Single-node mounts use [`open_isolated_store`].

use crate::err::to_errno;
use crate::io_buf::{apply_write, read_slice};
use crate::posix_lock::LockTable;
use crate::xattr::{self, SizedBytes};
use arkfs_core::attr_map::{merge_from_fuse, to_fuse, FuseSetAttr};
use arkfs_core::{
    allows, inherit_from_parent, sticky_allows_unlink, ArkError, FileType, PathKey, QuorumPolicy,
    Timespec, ACCESS_R, ACCESS_W, ACCESS_X,
};
use fuser::{FileAttr, FileType as FuseType, TimeOrNow, FUSE_ROOT_ID};
use persistent_object_store::open_isolated_store;
use std::collections::HashMap;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use temporal_core::{DirEntry, FileHandle, TemporalCore, View};

/// Kernel attribute cache TTL. Zero so nlink / size are not cached across link/unlink.
pub const TTL: Duration = Duration::ZERO;

/// Cap on a regular file's in-memory object (whole-file cache). Larger writes are EFBIG.
pub const MAX_FILE_BYTES: u64 = 1 << 30;

/// FUSE `statfs` numbers. `bsize`/`frsize` are 4096; `files` is the live path count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsStat {
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub bsize: u32,
    pub namelen: u32,
    pub frsize: u32,
}

/// One kernel file handle. `writable` is the open flags, not the mount.
struct OpenHandle {
    ino: u64,
    writable: bool,
}

/// Shared whole-object buffer for one inode. All fhs on that inode see it.
struct InodeBuf {
    path: PathKey,
    data: Vec<u8>,
    dirty: bool,
    loaded: bool,
    refs: u32,
    /// Last live name is gone (unlinked / overwritten). Bytes stay until release.
    unlinked: bool,
    file_type: FileType,
    /// Last getattr snapshot; used for fstat after the name is gone.
    stat: FileAttr,
}

/// In-process ArkFS node used by FUSE and by unit tests.
pub struct ArkSession {
    core: TemporalCore,
    view: View,
    read_only: bool,
    handles: Mutex<HashMap<u64, OpenHandle>>,
    inodes: Mutex<HashMap<u64, InodeBuf>>,
    next_fh: Mutex<u64>,
    locks: Arc<LockTable>,
}

impl ArkSession {
    /// Open `--data DIR` with no peer replication. `as_of_logical` is read-only.
    ///
    /// Maintainer: for normal FUSE mounts. ensure_root=true creates `/` as
    /// inode 1. For --as-of the session is read-only. See mount_store_inner.
    pub fn mount_store(data_dir: &Path, as_of_logical: Option<u64>) -> Result<Self, ArkError> {
        Self::mount_store_inner(data_dir, as_of_logical, true)
    }

    /// Open an existing store for integrity checks without creating `/`.
    ///
    /// Maintainer: used by fsck/inspect paths. Does not ensure root so that
    /// a corrupted or empty store can still be examined.
    pub fn inspect_store(data_dir: &Path) -> Result<Self, ArkError> {
        Self::mount_store_inner(data_dir, Some(0), false)
    }

    /// Internal mount helper.
    ///
    /// Maintainer: single place that opens an isolated store and TemporalCore.
    /// ensure_root controls whether we synthesize inode 1 for `/`.
    /// as_of_logical=None means Live (writable), Some means read-only historical view.
    fn mount_store_inner(
        data_dir: &Path,
        as_of_logical: Option<u64>,
        ensure_root: bool,
    ) -> Result<Self, ArkError> {
        let store = open_isolated_store(data_dir)?;
        let core = TemporalCore::open(store, QuorumPolicy::OwnerOnly)?;
        if ensure_root {
            let uid = unsafe { libc::getuid() };
            let gid = unsafe { libc::getgid() };
            core.ensure_root_as(uid, gid)?;
        }
        let (view, read_only) = match as_of_logical {
            Some(logical) => (
                View::AsOf(arkfs_core::Timestamp::new(logical, u64::MAX)),
                true,
            ),
            None => (View::Live, false),
        };
        Ok(ArkSession {
            core,
            view,
            read_only,
            handles: Mutex::new(HashMap::new()),
            inodes: Mutex::new(HashMap::new()),
            next_fh: Mutex::new(1),
            locks: Arc::new(LockTable::default()),
        })
    }

    /// True for `--as-of` mounts. Mutators return [`ArkError::ReadOnly`].
    ///
    /// Maintainer: mutating FUSE ops must call ro() early and return ReadOnly.
    /// See "GitHub vs local" for CI vs live FUSE behavior.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Current hybrid-logical tick. `--as-of N` is this number, not wall-clock.
    ///
    /// Maintainer: the logical component from TemporalCore. Used for
    /// client-visible "now". See "Logical time".
    pub fn logical_now(&self) -> u64 {
        self.core.now().logical
    }

    /// Reject mutators on an `--as-of` (read-only) mount.
    ///
    /// Maintainer: every mutating entry point must call this first.
    /// See "ReadOnly" error and is_read_only.
    fn ro(&self) -> Result<(), ArkError> {
        if self.read_only {
            Err(ArkError::ReadOnly)
        } else {
            Ok(())
        }
    }

    /// Unix bits plus stored ACLs for `mask` (POSIX `R_OK`/`W_OK`/`X_OK`/`F_OK`).
    ///
    /// Maintainer: permission model is in arkfs_core::access. FUSE layer
    /// translates to EACCES via to_errno. See "Unix permission bits".
    fn require_mode(&self, h: &FileHandle, uid: u32, gid: u32, mask: u32) -> Result<(), ArkError> {
        if allows(&h.attrs, uid, gid, mask) {
            Ok(())
        } else {
            Err(ArkError::permission_denied(h.path.as_str()))
        }
    }

    /// Relatime: persist atime if it is older than mtime or older than a day.
    ///
    /// Maintainer: only on Live (not read-only mounts). Best-effort; errors
    /// are swallowed. See "Relatime updates atime on read".
    fn maybe_touch_atime(&self, path: &str) {
        if self.read_only {
            return;
        }
        let Ok(h) = self.core.lookup(path, View::Live) else {
            return;
        };
        let now = time_or_now_to_timespec(TimeOrNow::Now);
        let day_ns = 86_400u64.saturating_mul(1_000_000_000);
        let stale = h.attrs.atime < h.attrs.mtime
            || now.as_nanos().saturating_sub(h.attrs.atime.as_nanos()) > day_ns;
        if !stale {
            return;
        }
        let _ = self.core.commit_attrs_now(path, |a| a.atime = now);
    }

    /// Directory W+X, plus sticky-bit owner check when `child` is given.
    ///
    /// Maintainer: used for create/unlink/rmdir/rename. Sticky bit check
    /// prevents non-owner from unlinking other people's entries in sticky dirs.
    /// See "sticky_allows_unlink".
    fn require_dir_write(
        &self,
        parent: &FileHandle,
        child: Option<&FileHandle>,
        uid: u32,
        gid: u32,
    ) -> Result<(), ArkError> {
        self.require_mode(parent, uid, gid, ACCESS_W | ACCESS_X)?;
        if let Some(child) = child {
            if !sticky_allows_unlink(parent.attrs.mode, parent.attrs.uid, child.attrs.uid, uid) {
                return Err(ArkError::permission_denied(child.path.as_str()));
            }
        }
        Ok(())
    }

    /// Lookup `name` in FUSE parent inode using the session's live/as-of view.
    ///
    /// Parent search (`X_OK`) is required. `uid`/`gid` 0 is root (tests).
    ///
    /// Maintainer: permission check is done here (X_OK on parent). Path
    /// construction uses PathKey::join. Core lookup then hides tombstones.
    pub fn lookup(
        &self,
        parent: u64,
        name: &str,
        uid: u32,
        gid: u32,
    ) -> Result<FileHandle, ArkError> {
        let parent = self.core.lookup_ino(parent, self.view)?;
        self.require_mode(&parent, uid, gid, ACCESS_X)?;
        let path = parent.path.join(name)?;
        self.core.lookup(path.as_str(), self.view)
    }

    /// Resolve a FUSE inode in the session view (O(1) live map; AsOf still scans).
    pub fn lookup_ino(&self, ino: u64) -> Result<FileHandle, ArkError> {
        self.core.lookup_ino(ino, self.view)
    }

    /// Stat. Dirty inode size is visible to every fh on that inode.
    ///
    /// After the last name is unlinked, `lookup_ino` fails; an open fh still
    /// answers from the cached [`FileAttr`] (POSIX `fstat` on an unlinked fd).
    pub fn getattr(&self, ino: u64, fh: Option<u64>) -> Result<FileAttr, ArkError> {
        let cache_ino = fh
            .and_then(|fh| self.handles.lock().unwrap().get(&fh).map(|h| h.ino))
            .unwrap_or(ino);
        match self.core.lookup_ino(cache_ino, self.view) {
            Ok(h) => {
                let mut attr = handle_to_attr(&h);
                self.overlay_cached_size(cache_ino, &mut attr);
                Ok(attr)
            }
            Err(e) => self.getattr_cached(cache_ino).ok_or(e),
        }
    }

    /// `..` inode for a directory (root's parent is itself).
    pub fn parent_ino(&self, ino: u64) -> Result<u64, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        Ok(h.path
            .parent()
            .and_then(|p| self.core.lookup(p.as_str(), self.view).ok())
            .map(|p| {
                if p.path.is_root() {
                    FUSE_ROOT_ID
                } else {
                    p.attrs.file_id
                }
            })
            .unwrap_or(FUSE_ROOT_ID))
    }

    /// Partial setattr. Size hits the shared inode buffer; other fields use attr_map.
    ///
    /// Truncate needs write. chmod/times need owner or root. chown needs root.
    ///
    /// Maintainer: must use merge_from_fuse (attr_map) — never wholesale replace
    /// FileAttributes. Size path goes through the inode buffer or replace_content.
    /// See "Partial setattr" trap.
    pub fn setattr(
        &self,
        ino: u64,
        patch: FuseSetAttr,
        fh: Option<u64>,
        uid: u32,
        gid: u32,
    ) -> Result<FileAttr, ArkError> {
        self.ro()?;
        let h = self.core.lookup_ino(ino, View::Live)?;
        if let Some(size) = patch.size {
            self.require_mode(&h, uid, gid, ACCESS_W)?;
            if size > MAX_FILE_BYTES {
                return Err(ArkError::file_too_large(format!(
                    "truncate {size} exceeds cap {MAX_FILE_BYTES}"
                )));
            }
        }
        if patch.mode.is_some() && uid != 0 && uid != h.attrs.uid {
            return Err(ArkError::permission_denied(h.path.as_str()));
        }
        if (patch.uid.is_some() || patch.gid.is_some()) && uid != 0 {
            return Err(ArkError::permission_denied(h.path.as_str()));
        }
        if (patch.atime.is_some() || patch.mtime.is_some() || patch.ctime.is_some())
            && uid != 0
            && uid != h.attrs.uid
        {
            return Err(ArkError::permission_denied(h.path.as_str()));
        }
        if let Some(size) = patch.size {
            if fh.is_some() || self.inodes.lock().unwrap().contains_key(&ino) {
                self.truncate_ino(ino, size)?;
            } else {
                let mut data = self.core.read_content(&h)?;
                data.resize(size as usize, 0);
                self.core.replace_content(h.path.as_str(), data)?;
            }
        }
        let posix = FuseSetAttr {
            size: None,
            ..patch
        };
        if posix.mode.is_some()
            || posix.uid.is_some()
            || posix.gid.is_some()
            || posix.atime.is_some()
            || posix.mtime.is_some()
            || posix.ctime.is_some()
        {
            let now = self.core.now().to_timespec();
            self.core.commit_attrs_now(h.path.as_str(), |attrs| {
                merge_from_fuse(attrs, &posix, now);
            })?;
        }
        self.getattr(ino, fh)
    }

    /// Create a live directory. `mode` is permission bits only (type comes from mkdir).
    ///
    /// Maintainer: calls into TemporalCore::mkdir which allocates file_id if 0
    /// (Inode 0 trap). Inherits setgid etc via inherit_from_parent. Permission
    /// via require_dir_write (W+X + sticky). See "mkdir" and "setgid directories".
    pub fn mkdir(
        &self,
        parent: u64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<FileHandle, ArkError> {
        self.ro()?;
        let parent = self.core.lookup_ino(parent, View::Live)?;
        self.require_dir_write(&parent, None, uid, gid)?;
        let (gid, mode) = inherit_from_parent(parent.attrs.mode, parent.attrs.gid, gid, mode, true);
        self.core.mkdir(parent.path.as_str(), name, mode, uid, gid)
    }

    /// Create an empty regular file and open it. Recreate after a tombstone is allowed.
    ///
    /// Maintainer: open-file cache is per-inode. The returned fh shares the
    /// InodeBuf with any other open handle on the same file_id. See
    /// "Open-file cache is per-inode" trap and release/flush paths.
    pub fn create_file(
        &self,
        parent: u64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
        flags: i32,
    ) -> Result<(FileHandle, u64), ArkError> {
        self.ro()?;
        let parent = self.core.lookup_ino(parent, View::Live)?;
        self.require_dir_write(&parent, None, uid, gid)?;
        let (gid, mode) = inherit_from_parent(
            parent.attrs.mode,
            parent.attrs.gid,
            gid,
            mode & 0o7777,
            false,
        );
        let h = self
            .core
            .create_file(parent.path.as_str(), name, mode, uid, gid)?;
        let fh = self.open_handle(&h, flags, true)?;
        Ok((h, fh))
    }

    /// Tombstone a non-directory. Flushes the inode first so dirty bytes are history.
    ///
    /// Maintainer: must flush_ino before tombstone so dirty writes become a
    /// version. Then syncs inode paths. See "flush before unlink" and
    /// "release must persist".
    pub fn unlink(&self, parent: u64, name: &str, uid: u32, gid: u32) -> Result<(), ArkError> {
        self.ro()?;
        let dir = self.core.lookup_ino(parent, View::Live)?;
        let path = dir.path.join(name)?;
        let child = self.core.lookup(path.as_str(), View::Live)?;
        self.require_dir_write(&dir, Some(&child), uid, gid)?;
        self.flush_ino(child.attrs.file_id)?;
        self.core.unlink(path.as_str())?;
        self.sync_inode_paths();
        Ok(())
    }

    /// Tombstone an empty directory. Root cannot be removed.
    ///
    /// Maintainer: directory emptiness is checked at tombstone time.
    /// rmdir is just a tombstone (never-delete). See "rmdir".
    pub fn rmdir(&self, parent: u64, name: &str, uid: u32, gid: u32) -> Result<(), ArkError> {
        self.ro()?;
        let dir = self.core.lookup_ino(parent, View::Live)?;
        let path = dir.path.join(name)?;
        let child = self.core.lookup(path.as_str(), View::Live)?;
        self.require_dir_write(&dir, Some(&child), uid, gid)?;
        self.core.rmdir(path.as_str())?;
        self.sync_inode_paths();
        Ok(())
    }

    /// Move a name. Directories take every descendant. `flags` are `renameat2` bits.
    ///
    /// Maintainer: delegates to core rename which uses rebase + tombstone
    /// fan-out in one persist. Directory rename retargets all live descendants
    /// (Directory rename trap). Must call sync_inode_paths after.
    /// Also flushes the source inode.
    #[allow(clippy::too_many_arguments)]
    pub fn rename(
        &self,
        parent: u64,
        name: &str,
        newparent: u64,
        newname: &str,
        flags: u32,
        uid: u32,
        gid: u32,
    ) -> Result<(), ArkError> {
        self.ro()?;
        let from_dir = self.core.lookup_ino(parent, View::Live)?;
        let from = from_dir.path.join(name)?;
        let src = self.core.lookup(from.as_str(), View::Live)?;
        self.require_dir_write(&from_dir, Some(&src), uid, gid)?;
        self.flush_ino(src.attrs.file_id)?;
        if src.attrs.file_type == FileType::Directory {
            self.flush_under(&from)?;
        }
        let to_parent = self.core.lookup_ino(newparent, View::Live)?;
        let dest = to_parent.path.join(newname)?;
        let dest_child = self.core.lookup(dest.as_str(), View::Live).ok();
        self.require_dir_write(&to_parent, dest_child.as_ref(), uid, gid)?;
        self.core
            .rename(from.as_str(), to_parent.path.as_str(), newname, flags)?;
        self.sync_inode_paths();
        Ok(())
    }

    /// Extra directory entry for a live non-directory (hard link).
    ///
    /// Maintainer: must flush the source inode first (dirty data becomes
    /// history). Hard links share file_id; writes fan out. See "link" and
    /// "Open-file cache is per-inode".
    pub fn link(
        &self,
        ino: u64,
        newparent: u64,
        newname: &str,
        uid: u32,
        gid: u32,
    ) -> Result<FileHandle, ArkError> {
        self.ro()?;
        let src = self.core.lookup_ino(ino, View::Live)?;
        self.flush_ino(src.attrs.file_id)?;
        let parent = self.core.lookup_ino(newparent, View::Live)?;
        self.require_dir_write(&parent, None, uid, gid)?;
        self.core
            .link(src.path.as_str(), parent.path.as_str(), newname)
    }

    /// Create a symlink. Target is stored in attrs and as content bytes.
    ///
    /// Maintainer: target bytes are both in symlink_target attr and as the
    /// content object (for readlink fallback). See TemporalCore::symlink.
    pub fn symlink(
        &self,
        parent: u64,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> Result<FileHandle, ArkError> {
        self.ro()?;
        let parent = self.core.lookup_ino(parent, View::Live)?;
        self.require_dir_write(&parent, None, uid, gid)?;
        let (gid, _) = inherit_from_parent(parent.attrs.mode, parent.attrs.gid, gid, 0o777, false);
        self.core
            .symlink(parent.path.as_str(), name, target, uid, gid)
    }

    /// Read a symlink target. Prefers `attrs.symlink_target`, else content bytes.
    ///
    /// Maintainer: both locations are written by TemporalCore::symlink so that
    /// readlink works even if attrs projection changes. See "symlink".
    pub fn readlink(&self, ino: u64) -> Result<Vec<u8>, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        match h.attrs.symlink_target {
            Some(t) => Ok(t.into_bytes()),
            None => self.core.read_content(&h),
        }
    }

    /// Immediate children. FUSE adds `.` / `..` itself. Needs directory R+X.
    ///
    /// Maintainer: delegates to TemporalCore::readdir which is a path-prefix
    /// scan (not inode children). See "readdir is a prefix scan".
    pub fn readdir(&self, ino: u64, uid: u32, gid: u32) -> Result<Vec<DirEntry>, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        if h.attrs.file_type != FileType::Directory {
            return Err(ArkError::not_a_directory(h.path.as_str()));
        }
        self.require_mode(&h, uid, gid, ACCESS_R | ACCESS_X)?;
        let entries = self.core.readdir(h.path.as_str(), self.view)?;
        self.maybe_touch_atime(h.path.as_str());
        Ok(entries)
    }

    /// Open a directory. Must be a directory; search (`X_OK`) required.
    ///
    /// Maintainer: directories are stateless (fh is a dummy 0). Permission
    /// is X_OK only. See "opendir".
    pub fn opendir(&self, ino: u64, uid: u32, gid: u32) -> Result<u64, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        if h.attrs.file_type != FileType::Directory {
            return Err(ArkError::not_a_directory(h.path.as_str()));
        }
        self.require_mode(&h, uid, gid, ACCESS_X)?;
        Ok(0)
    }

    /// Open a non-directory. Directories go through `opendir` (stateless).
    ///
    /// Fifo / char / block / socket return [`ArkError::NoSuchDevice`] (`ENXIO`):
    /// this node does not host a pipe or device driver.
    ///
    /// Maintainer: returns a per-inode fh. Writable opens go through ro().
    /// See "open-file cache is per-inode" and release/flush.
    pub fn open(&self, ino: u64, flags: i32, uid: u32, gid: u32) -> Result<u64, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        if h.attrs.file_type == FileType::Directory {
            return Err(ArkError::is_a_directory(h.path.as_str()));
        }
        if matches!(
            h.attrs.file_type,
            FileType::Fifo | FileType::CharDevice | FileType::BlockDevice | FileType::Socket
        ) {
            return Err(ArkError::no_such_device(h.path.as_str()));
        }
        self.require_mode(&h, uid, gid, open_mask(flags))?;
        let writable = is_writable(flags) && h.attrs.file_type == FileType::File;
        if writable {
            self.ro()?;
        } else if is_writable(flags) && h.attrs.file_type != FileType::File {
            return Err(ArkError::invalid_argument("write to non-regular file"));
        }
        self.open_handle(&h, flags, writable)
    }

    /// Unix `access(2)` against a live inode (`mask` is `R_OK`/`W_OK`/`X_OK`/`F_OK`).
    ///
    /// Maintainer: delegates to require_mode which uses arkfs_core::allows.
    /// F_OK is always true once the inode was found.
    pub fn access(&self, ino: u64, mask: i32, uid: u32, gid: u32) -> Result<(), ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        self.require_mode(&h, uid, gid, mask as u32)
    }

    /// Re-hash every published CAS object. Used by `arkfs fsck`.
    ///
    /// Maintainer: delegates to PersistentObjectStore::verify_integrity.
    /// Failures are Integrity errors (fail closed). See "verify_integrity".
    pub fn verify_integrity(&self) -> Result<persistent_object_store::IntegrityReport, ArkError> {
        self.core.store().verify_integrity()
    }

    /// Live path count + on-disk object bytes + backing-fs free space.
    ///
    /// Maintainer: files = live_path_count (non-tombstones). Blocks from
    /// backing fs or synthetic. See "statfs" and usage().
    pub fn statfs(&self) -> Result<FsStat, ArkError> {
        let (_objs, bytes) = self.core.store().usage()?;
        let files = self.core.live_path_count();
        const BSIZE: u32 = 4096;
        let used = bytes.div_ceil(BSIZE as u64).max(1);
        let (blocks, bfree, bavail) = backing_blocks(self.core.store().root(), BSIZE, used);
        Ok(FsStat {
            blocks,
            bfree,
            bavail,
            files,
            ffree: u64::MAX / 4,
            bsize: BSIZE,
            namelen: 255,
            frsize: BSIZE,
        })
    }

    /// Read from the shared inode buffer. Negative offsets behave as 0.
    ///
    /// Maintainer: content comes from the per-inode InodeBuf (dirty or loaded
    /// from CAS). atime update is best-effort (relatime). See "read" and
    /// "maybe_touch_atime".
    pub fn read(&self, fh: u64, offset: i64, size: u32) -> Result<Vec<u8>, ArkError> {
        let ino = self.fh_ino(fh)?;
        let mut inodes = self.inodes.lock().unwrap();
        let buf = inodes
            .get_mut(&ino)
            .ok_or_else(|| ArkError::invalid_argument("bad fh"))?;
        self.ensure_loaded(ino, buf)?;
        let out = read_slice(&buf.data, offset, size).to_vec();
        let path = buf.path.as_str().to_string();
        drop(inodes);
        self.maybe_touch_atime(&path);
        Ok(out)
    }

    /// Write into the shared inode buffer. Regular files only; persist on fsync/release.
    ///
    /// Maintainer: writes are buffered per-inode (InodeBuf). All fhs on the
    /// same inode share the buffer. Persistence happens on fsync/release.
    /// See "Open-file cache is per-inode" trap and flush_ino/release.
    pub fn write(&self, fh: u64, offset: i64, data: &[u8]) -> Result<u32, ArkError> {
        self.ro()?;
        let (ino, writable) = {
            let handles = self.handles.lock().unwrap();
            let h = handles
                .get(&fh)
                .ok_or_else(|| ArkError::invalid_argument("bad fh"))?;
            (h.ino, h.writable)
        };
        if !writable {
            return Err(ArkError::ReadOnly);
        }
        let mut inodes = self.inodes.lock().unwrap();
        let buf = inodes
            .get_mut(&ino)
            .ok_or_else(|| ArkError::invalid_argument("bad fh"))?;
        if buf.file_type != FileType::File {
            return Err(ArkError::invalid_argument("not a regular file"));
        }
        self.ensure_loaded(ino, buf)?;
        let start = u64::try_from(offset.max(0)).unwrap_or(u64::MAX);
        let end = start.saturating_add(data.len() as u64);
        if end > MAX_FILE_BYTES {
            return Err(ArkError::file_too_large(format!(
                "write would be {end} bytes (cap {MAX_FILE_BYTES})"
            )));
        }
        apply_write(&mut buf.data, offset, data);
        buf.dirty = true;
        buf.stat.size = buf.data.len() as u64;
        buf.stat.blocks = buf.stat.size.div_ceil(512);
        Ok(data.len() as u32)
    }

    /// Persist dirty bytes for this fh's inode. Does not drop the fh or locks.
    ///
    /// Maintainer: open-file cache is per-inode. fsync must persist the
    /// whole buffer before returning. On error the buffer must remain dirty.
    /// See "fsync / flush / release must persist" and "Open-file cache is per-inode".
    pub fn fsync(&self, fh: u64) -> Result<(), ArkError> {
        let ino = self.fh_ino(fh)?;
        self.flush_ino(ino)
    }

    /// Persist dirty bytes then drop the fh. On persist failure the buffer stays.
    ///
    /// Maintainer: release must attempt to persist. If persist fails, keep the
    /// buffer (refs, dirty) and return the error so the application can retry.
    /// This is the critical "release must persist" durability rule.
    /// See "release must persist before dropping the fh" trap.
    pub fn release(&self, fh: u64) -> Result<(), ArkError> {
        let ino = self.fh_ino(fh)?;
        let flush_res = self.flush_ino(ino);
        self.handles.lock().unwrap().remove(&fh);
        let mut inodes = self.inodes.lock().unwrap();
        if let Some(buf) = inodes.get_mut(&ino) {
            buf.refs = buf.refs.saturating_sub(1);
            if buf.refs == 0 && (!buf.dirty || buf.unlinked) {
                inodes.remove(&ino);
            }
        }
        flush_res
    }

    /// Retry persist for every dirty inode (unmount / destroy).
    ///
    /// Maintainer: called from FuseFs::destroy. Best-effort; records last error.
    /// See "destroy" and "flush_dirty".
    pub fn flush_dirty(&self) -> Result<(), ArkError> {
        let inos: Vec<u64> = self
            .inodes
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, buf)| buf.dirty)
            .map(|(ino, _)| *ino)
            .collect();
        let mut last = Ok(());
        for ino in inos {
            if let Err(e) = self.flush_ino(ino) {
                last = Err(e);
            }
        }
        last
    }

    /// Set one xattr. Fans out to every hard-link name of the inode.
    ///
    /// Maintainer: uses commit_attrs (which fans out via hard links). Respects
    /// XATTR_CREATE / REPLACE. Permission is W on the inode.
    /// See "xattr" and "XATTR_CREATE / XATTR_REPLACE".
    pub fn setxattr(
        &self,
        ino: u64,
        name: &str,
        value: &[u8],
        flags: i32,
        uid: u32,
        gid: u32,
    ) -> Result<(), ArkError> {
        self.ro()?;
        let h = self.core.lookup_ino(ino, View::Live)?;
        self.require_mode(&h, uid, gid, ACCESS_W)?;
        let exists = h.attrs.xattrs.contains_key(name);
        if flags == libc::XATTR_CREATE && exists {
            return Err(ArkError::already_exists(name));
        }
        if flags == libc::XATTR_REPLACE && !exists {
            return Err(ArkError::not_found(name));
        }
        self.core.commit_attrs_now(h.path.as_str(), |attrs| {
            attrs.xattrs.insert(name.to_string(), value.to_vec());
        })?;
        Ok(())
    }

    /// Get one xattr. Missing name is [`ArkError::NotFound`] (FUSE maps to ENODATA).
    pub fn getxattr(&self, ino: u64, name: &str) -> Result<Vec<u8>, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        h.attrs
            .xattrs
            .get(name)
            .cloned()
            .ok_or_else(|| ArkError::not_found(name))
    }

    /// FUSE getxattr size protocol: `size == 0` → length, else data or ERANGE.
    pub fn getxattr_sized(&self, ino: u64, name: &str, size: u32) -> Result<SizedBytes, ArkError> {
        Ok(xattr::sized(&self.getxattr(ino, name)?, size))
    }

    /// NUL-separated xattr name list (FUSE `listxattr` payload).
    pub fn listxattr(&self, ino: u64) -> Result<Vec<u8>, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        Ok(xattr::encode_list(
            h.attrs.xattrs.keys().map(String::as_str),
        ))
    }

    /// FUSE listxattr size protocol. Same rules as [`Self::getxattr_sized`].
    pub fn listxattr_sized(&self, ino: u64, size: u32) -> Result<SizedBytes, ArkError> {
        Ok(xattr::sized(&self.listxattr(ino)?, size))
    }

    /// Remove one xattr. Fans out to every hard-link name.
    pub fn removexattr(&self, ino: u64, name: &str, uid: u32, gid: u32) -> Result<(), ArkError> {
        self.ro()?;
        let h = self.core.lookup_ino(ino, View::Live)?;
        self.require_mode(&h, uid, gid, ACCESS_W)?;
        self.core.commit_attrs_now(h.path.as_str(), |attrs| {
            attrs.xattrs.remove(name);
        })?;
        Ok(())
    }

    /// Create a node of any POSIX type. `mode` includes `S_IFMT`; `rdev` is for devices.
    ///
    /// Maintainer: delegates to core mknod which goes through commit_branch
    /// (Inode 0 allocation + conflict-before-write + safe publish).
    /// Directories are redirected to mkdir. See "mknod".
    #[allow(clippy::too_many_arguments)]
    pub fn mknod(
        &self,
        parent: u64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
        rdev: u32,
    ) -> Result<FileHandle, ArkError> {
        self.ro()?;
        let parent = self.core.lookup_ino(parent, View::Live)?;
        self.require_dir_write(&parent, None, uid, gid)?;
        let is_dir = matches!(mode & libc::S_IFMT, libc::S_IFDIR);
        let (gid, mode) =
            inherit_from_parent(parent.attrs.mode, parent.attrs.gid, gid, mode, is_dir);
        let file_type = mode_to_file_type(mode)?;
        let rdev = match file_type {
            FileType::BlockDevice | FileType::CharDevice => Some(rdev as u64),
            _ => None,
        };
        self.core.mknod(
            parent.path.as_str(),
            name,
            file_type,
            mode & 0o7777,
            uid,
            gid,
            rdev,
        )
    }

    /// Copy `len` bytes between two open handles (POSIX `copy_file_range`).
    ///
    /// Source bytes are cloned first so same-file overlapping copies are
    /// memmove-safe. `flags` must be 0.
    ///
    /// Maintainer: implemented via read into a temp buffer then write.
    /// Both source and dest must be open regular files. No cross-device.
    /// See "copy_file_range".
    #[allow(clippy::too_many_arguments)]
    pub fn copy_file_range(
        &self,
        fh_in: u64,
        offset_in: i64,
        fh_out: u64,
        offset_out: i64,
        len: u64,
        flags: u32,
    ) -> Result<u32, ArkError> {
        self.ro()?;
        if flags != 0 {
            return Err(ArkError::invalid_argument("copy_file_range flags"));
        }
        if offset_in < 0 || offset_out < 0 {
            return Err(ArkError::invalid_argument("negative copy offset"));
        }
        if len == 0 {
            return Ok(0);
        }
        let take = len.min(u32::MAX as u64) as u32;
        let ino_in = self.fh_ino(fh_in)?;
        let chunk = {
            let mut inodes = self.inodes.lock().unwrap();
            let buf = inodes
                .get_mut(&ino_in)
                .ok_or_else(|| ArkError::invalid_argument("bad fh"))?;
            self.ensure_loaded(ino_in, buf)?;
            read_slice(&buf.data, offset_in, take).to_vec()
        };
        self.write(fh_out, offset_out, &chunk)
    }

    /// F_GETLK: return the conflicting lock, or `(start, end, F_UNLCK, pid)`.
    #[allow(clippy::too_many_arguments)]
    pub fn getlk(
        &self,
        ino: u64,
        owner: u64,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
    ) -> Result<(u64, u64, i32, u32), ArkError> {
        self.require_lockable(ino)?;
        Ok(self.locks.getlk(ino, owner, start, end, typ, pid))
    }

    /// F_SETLK / F_SETLKW. `wait` sleeps on a condvar; the FUSE adapter must
    /// reply from another thread so the session loop can process the unlock.
    #[allow(clippy::too_many_arguments)]
    pub fn setlk(
        &self,
        ino: u64,
        owner: u64,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
        wait: bool,
    ) -> Result<(), ArkError> {
        self.require_lockable(ino)?;
        self.locks.setlk(ino, owner, start, end, typ, pid, wait)
    }

    /// Shared lock table for SETLKW helper threads (same crate only).
    pub(crate) fn lock_table(&self) -> Arc<LockTable> {
        Arc::clone(&self.locks)
    }

    /// Drop every fcntl lock `owner` holds on `ino` (FUSE `flush` / last close).
    pub fn unlock_owner(&self, ino: u64, owner: u64) {
        self.locks.unlock_owner(ino, owner);
    }

    /// `flush` is FUSE close: persist dirty bytes and drop POSIX locks for `owner`.
    ///
    /// Maintainer: called on every close(2) of an fd. Must persist. Locks for
    /// the lock_owner are dropped here. See "flush" and "POSIX locks".
    pub fn flush(&self, fh: u64, lock_owner: u64) -> Result<(), ArkError> {
        let ino = self.fh_ino(fh)?;
        self.locks.unlock_owner(ino, lock_owner);
        self.flush_ino(ino)
    }

    /// Local files are always readable/writable. `events` is the requested mask.
    ///
    /// Maintainer: poll always reports POLLIN|POLLOUT etc. for regular files.
    /// No real async I/O. See "poll" and "always-ready poll".
    pub fn poll(&self, ino: u64, fh: Option<u64>, events: u32) -> Result<u32, ArkError> {
        if let Some(fh) = fh {
            if fh != 0 {
                let _ = self.fh_ino(fh)?;
            }
        }
        let _ = self.getattr(ino, fh)?;
        let ready = (libc::POLLIN | libc::POLLOUT | libc::POLLRDNORM | libc::POLLWRNORM) as u32;
        Ok(events & ready)
    }

    /// Identity block map (not a blkdev). `idx` is returned unchanged.
    pub fn bmap(&self, ino: u64, blocksize: u32, idx: u64) -> Result<u64, ArkError> {
        let _ = self.core.lookup_ino(ino, self.view)?;
        if blocksize == 0 {
            return Err(ArkError::invalid_argument("bmap blocksize"));
        }
        Ok(idx)
    }

    /// Directory listing plus attributes (FUSE `readdirplus`).
    /// Returns `(entry, attr, generation)` for each child. No `.` / `..`.
    pub fn readdir_plus(
        &self,
        ino: u64,
        uid: u32,
        gid: u32,
    ) -> Result<Vec<(DirEntry, FileAttr, u64)>, ArkError> {
        let entries = self.readdir(ino, uid, gid)?;
        let mut out = Vec::with_capacity(entries.len());
        for e in entries {
            let h = self.core.lookup_ino(e.ino, self.view)?;
            let mut attr = handle_to_attr(&h);
            self.overlay_cached_size(e.ino, &mut attr);
            out.push((e, attr, h.attrs.generation));
        }
        Ok(out)
    }

    /// Grow a regular file. Only mode 0 and `FALLOC_FL_KEEP_SIZE` are accepted.
    ///
    /// Maintainer: with KEEP_SIZE we are a no-op (we do not allocate). Without
    /// it we extend via setattr (which may dirty the inode buffer).
    /// See "fallocate".
    pub fn fallocate(
        &self,
        ino: u64,
        fh: Option<u64>,
        offset: i64,
        length: i64,
        mode: i32,
    ) -> Result<(), ArkError> {
        self.ro()?;
        if offset < 0 || length < 0 {
            return Err(ArkError::invalid_argument("fallocate range"));
        }
        const KEEP_SIZE: i32 = libc::FALLOC_FL_KEEP_SIZE;
        if mode != 0 && mode != KEEP_SIZE {
            return Err(ArkError::not_implemented("fallocate mode"));
        }
        if mode == KEEP_SIZE {
            return Ok(());
        }
        let end = (offset as u64).saturating_add(length as u64);
        let cur = self.getattr(ino, fh)?.size;
        if end > cur {
            self.setattr(
                ino,
                FuseSetAttr {
                    size: Some(end),
                    ..Default::default()
                },
                fh,
                0,
                0,
            )?;
        }
        Ok(())
    }

    /// Reposition. Whole-object store: `SEEK_DATA` is the offset, `SEEK_HOLE` is EOF.
    pub fn lseek(
        &self,
        ino: u64,
        fh: Option<u64>,
        offset: i64,
        whence: i32,
    ) -> Result<i64, ArkError> {
        let size = self.getattr(ino, fh)?.size as i64;
        let pos = match whence {
            libc::SEEK_SET => offset,
            libc::SEEK_END => size.saturating_add(offset),
            libc::SEEK_DATA => {
                if offset < 0 || offset >= size {
                    return Err(ArkError::invalid_argument("SEEK_DATA"));
                }
                offset
            }
            libc::SEEK_HOLE => {
                if offset < 0 || offset > size {
                    return Err(ArkError::invalid_argument("SEEK_HOLE"));
                }
                size
            }
            _ => return Err(ArkError::invalid_argument("lseek whence")),
        };
        if pos < 0 {
            return Err(ArkError::invalid_argument("negative seek"));
        }
        Ok(pos)
    }

    /// Map a kernel file handle to its inode. Unknown fh returns InvalidArgument.
    ///
    /// fhs are allocated in open_handle and are just keys. The authoritative
    /// state (data, dirty, path) lives in the per-inode InodeBuf map.
    ///
    /// Preconditions: fh was previously returned by open or create_file.
    /// Postconditions: returns the ino that owns the buffer for this fh.
    ///
    /// Maintainer: fhs are just keys into the handles table; the real state is
    /// the per-inode InodeBuf. See "fh" vs inode and "Open-file cache is per-inode".
    fn fh_ino(&self, fh: u64) -> Result<u64, ArkError> {
        self.handles
            .lock()
            .unwrap()
            .get(&fh)
            .map(|h| h.ino)
            .ok_or_else(|| ArkError::invalid_argument("bad fh"))
    }

    /// Reject fcntl locks on directories.
    ///
    /// Looks up the inode and returns InvalidArgument if it is a directory.
    /// Regular files (and most specials) are lockable.
    ///
    /// Preconditions: ino is visible in the view.
    /// Postconditions: Ok for files, error for directories.
    ///
    /// Maintainer: POSIX record locks are only meaningful on regular files.
    /// Directories return EINVAL. See posix_lock and "POSIX locks".
    fn require_lockable(&self, ino: u64) -> Result<(), ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        if h.attrs.file_type == FileType::Directory {
            return Err(ArkError::invalid_argument("fcntl lock on directory"));
        }
        Ok(())
    }

    /// Overlay the current dirty buffer size onto a FileAttr (if the inode is open).
    ///
    /// If the inode has a loaded InodeBuf, its .data.len() becomes the reported size
    /// and blocks is recomputed. Called from getattr when an fh is supplied and
    /// from readdir_plus. No-op when the inode has no open buffer.
    ///
    /// Preconditions: attr is a snapshot from core or cached getattr.
    /// Postconditions: size/blocks reflect the dirty buffer if present.
    ///
    /// Maintainer: used by getattr when an open fh is supplied. The per-inode
    /// buffer is authoritative for size while the file is open. See "getattr"
    /// and "Open-file cache is per-inode".
    fn overlay_cached_size(&self, ino: u64, attr: &mut FileAttr) {
        if let Some(buf) = self.inodes.lock().unwrap().get(&ino) {
            if buf.loaded {
                attr.size = buf.data.len() as u64;
                attr.blocks = attr.size.div_ceil(512);
            }
        }
    }

    /// Return a FileAttr from the open InodeBuf for an unlinked inode.
    ///
    /// Used after the last name is tombstoned but fhs remain. Forces nlink=0.
    /// Size comes from the dirty buffer if loaded. Returns None if no buffer.
    ///
    /// Preconditions: may be called after unlink while fhs are still open.
    /// Postconditions: attr reflects the last known stat + current dirty bytes.
    ///
    /// Maintainer: after unlink of the last name, fstat on an open fd must
    /// still work from the cached InodeBuf. nlink is forced to 0. See
    /// "getattr_cached" and "Open-file cache is per-inode".
    fn getattr_cached(&self, ino: u64) -> Option<FileAttr> {
        let inodes = self.inodes.lock().unwrap();
        let buf = inodes.get(&ino)?;
        let mut attr = buf.stat;
        if buf.loaded {
            attr.size = buf.data.len() as u64;
            attr.blocks = attr.size.div_ceil(512);
        }
        if buf.unlinked {
            attr.nlink = 0;
        }
        Some(attr)
    }

    /// Allocate an fh and attach it to the per-inode buffer. `O_TRUNC` is inode-wide.
    ///
    /// Maintainer: every open file handle shares the InodeBuf for its inode.
    /// O_TRUNC is applied at open time via replace_content. refs count
    /// controls when the buffer can be dropped. See "open-file cache is per-inode".
    fn open_handle(&self, h: &FileHandle, flags: i32, writable: bool) -> Result<u64, ArkError> {
        let ino = h.attrs.file_id;
        let path = h.path.clone();
        let file_type = h.attrs.file_type;
        let stat = handle_to_attr(h);
        let trunc = writable && (flags & libc::O_TRUNC) != 0;
        if trunc {
            self.core.replace_content(path.as_str(), Vec::new())?;
        }
        {
            let mut inodes = self.inodes.lock().unwrap();
            let buf = inodes.entry(ino).or_insert_with(|| InodeBuf {
                path: path.clone(),
                data: Vec::new(),
                dirty: false,
                loaded: false,
                refs: 0,
                unlinked: false,
                file_type,
                stat,
            });
            buf.path = path;
            buf.file_type = file_type;
            buf.stat = stat;
            buf.unlinked = false;
            buf.refs += 1;
            if trunc {
                buf.data.clear();
                buf.loaded = true;
                buf.dirty = false;
                buf.stat.size = 0;
                buf.stat.blocks = 0;
            }
        }
        let mut next = self.next_fh.lock().unwrap();
        let fh = *next;
        *next += 1;
        self.handles
            .lock()
            .unwrap()
            .insert(fh, OpenHandle { ino, writable });
        Ok(fh)
    }

    /// Load CAS bytes into `buf`. If the cached path is stale after rename/unlink, retry by inode.
    ///
    /// If buf is already loaded, no-op. On lookup failure for the stored path,
    /// falls back to lookup_ino so unlinked-but-open files still read their
    /// last content. Updates buf.data, stat, path and sets loaded=true.
    ///
    /// Preconditions: buf belongs to `ino`.
    /// Postconditions: buf.loaded is true and data reflects last committed content.
    ///
    /// Maintainer: called on first read/write after open or after rename/unlink
    /// retargeting. If the live name is gone we fall back to lookup by inode.
    /// See "ensure_loaded" and "getattr_cached".
    fn ensure_loaded(&self, ino: u64, buf: &mut InodeBuf) -> Result<(), ArkError> {
        if buf.loaded {
            return Ok(());
        }
        let h = match self.core.lookup(buf.path.as_str(), self.view) {
            Ok(h) => h,
            Err(_) => self.core.lookup_ino(ino, self.view)?,
        };
        buf.data = self.core.read_content(&h)?;
        buf.stat = handle_to_attr(&h);
        buf.path = h.path;
        buf.loaded = true;
        Ok(())
    }

    /// Resize the shared inode buffer (dirty), or persist a truncate when the inode has no open buffer.
    ///
    /// If an InodeBuf exists we resize it in place and mark dirty (no I/O).
    /// Otherwise we read the last content, resize, and call replace_content
    /// (which does commit_branch + persist). Used by setattr size path.
    ///
    /// Preconditions: ino is a regular file visible live.
    /// Postconditions: logical size changed; either buffer is dirty or a new version exists.
    ///
    /// Maintainer: if the inode has an open buffer we mutate it (dirty=true).
    /// Otherwise we go through replace_content (which does commit_branch).
    /// See "truncate" and "Open-file cache is per-inode".
    fn truncate_ino(&self, ino: u64, size: u64) -> Result<(), ArkError> {
        {
            let mut inodes = self.inodes.lock().unwrap();
            if let Some(buf) = inodes.get_mut(&ino) {
                self.ensure_loaded(ino, buf)?;
                buf.data.resize(size as usize, 0);
                buf.dirty = true;
                buf.stat.size = size;
                buf.stat.blocks = size.div_ceil(512);
                return Ok(());
            }
        }
        let h = self.core.lookup_ino(ino, View::Live)?;
        let mut data = self.core.read_content(&h)?;
        data.resize(size as usize, 0);
        self.core.replace_content(h.path.as_str(), data)?;
        Ok(())
    }

    /// Persist dirty regular-file bytes for this inode (the core of fsync/release).
    ///
    /// Clones the buffer under lock, drops the lock, then calls replace_content.
    /// On NotFound (name gone after unlink) it retries via lookup_ino. On any
    /// persist error the buffer stays dirty. Special files and non-dirty are no-ops.
    ///
    /// Preconditions: called while holding no index lock.
    /// Postconditions: on success dirty is cleared; on failure dirty remains.
    ///
    /// Maintainer: the heart of durability on close/fsync. Must succeed or
    /// keep the buffer dirty. On name disappearance after unlink, it may
    /// fall back to lookup_ino. See "release must persist" and "flush_ino".
    fn flush_ino(&self, ino: u64) -> Result<(), ArkError> {
        let (path, data) = {
            let mut inodes = self.inodes.lock().unwrap();
            let Some(buf) = inodes.get_mut(&ino) else {
                return Ok(());
            };
            if !buf.dirty || buf.unlinked {
                return Ok(());
            }
            if buf.file_type != FileType::File {
                buf.dirty = false;
                return Ok(());
            }
            (buf.path.clone(), buf.data.clone())
        };
        match self.core.replace_content(path.as_str(), data.clone()) {
            Ok(_) => {}
            Err(ArkError::NotFound { .. }) => match self.core.lookup_ino(ino, View::Live) {
                Ok(h) => {
                    self.core.replace_content(h.path.as_str(), data)?;
                    if let Some(buf) = self.inodes.lock().unwrap().get_mut(&ino) {
                        buf.path = h.path;
                    }
                }
                Err(_) => return Ok(()),
            },
            Err(e) => return Err(e),
        }
        if let Some(buf) = self.inodes.lock().unwrap().get_mut(&ino) {
            buf.dirty = false;
        }
        Ok(())
    }

    /// Flush every open inode whose cached path is under `prefix` (used before directory rename).
    ///
    /// Collects inodes whose PathKey is under the prefix, then calls flush_ino on each.
    /// Guarantees that any dirty data under a subtree being moved becomes a version
    /// before rebase changes the paths.
    ///
    /// Preconditions: called while holding the caller side of a rename.
    /// Postconditions: all matching buffers are non-dirty (or error).
    ///
    /// Maintainer: called before rename when the source subtree may have dirty
    /// buffers. Ensures data is a version before the tree move.
    /// See "flush_under" and "Directory rename" trap.
    fn flush_under(&self, prefix: &PathKey) -> Result<(), ArkError> {
        let inos: Vec<u64> = self
            .inodes
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, b)| b.path.is_under(prefix))
            .map(|(ino, _)| *ino)
            .collect();
        for ino in inos {
            self.flush_ino(ino)?;
        }
        Ok(())
    }

    /// After rename/unlink, retarget every open InodeBuf to a remaining live name (or mark unlinked).
    ///
    /// Iterates known inodes and does lookup_ino(Live). On success updates buf.path
    /// and clears unlinked. On failure (no live name) sets unlinked=true so later
    /// flush_ino can still succeed via the cached content for open fhs.
    ///
    /// Preconditions: called after a successful core rename or unlink that may have
    /// moved or removed names of open inodes.
    /// Postconditions: every open buffer's path reflects a live name or is marked unlinked.
    ///
    /// Maintainer: directory rename retargets open-file caches by inode
    /// (lookup_ino). See "Directory rename" and "Open-file cache is per-inode".
    fn sync_inode_paths(&self) {
        let inos: Vec<u64> = self.inodes.lock().unwrap().keys().copied().collect();
        for ino in inos {
            match self.core.lookup_ino(ino, View::Live) {
                Ok(h) => {
                    if let Some(buf) = self.inodes.lock().unwrap().get_mut(&ino) {
                        buf.path = h.path;
                        buf.unlinked = false;
                    }
                }
                Err(_) => {
                    if let Some(buf) = self.inodes.lock().unwrap().get_mut(&ino) {
                        buf.unlinked = true;
                    }
                }
            }
        }
    }
}

/// Map `stat.st_mode` `S_IFMT` bits to [`FileType`]. `0` means regular (mknod default).
///
/// Used by mknod/create paths before calling into TemporalCore. Unknown types
/// become InvalidArgument (never reach the index).
///
/// Preconditions: mode contains a POSIX file type or 0.
/// Postconditions: returns a FileType or error for unsupported S_IF*.
///
/// Maintainer: only regular/dir/symlink/device/fifo/socket are supported.
/// See mknod and "mknod file type".
fn mode_to_file_type(mode: u32) -> Result<FileType, ArkError> {
    match mode & libc::S_IFMT {
        0 | libc::S_IFREG => Ok(FileType::File),
        libc::S_IFDIR => Ok(FileType::Directory),
        libc::S_IFLNK => Ok(FileType::Symlink),
        libc::S_IFBLK => Ok(FileType::BlockDevice),
        libc::S_IFCHR => Ok(FileType::CharDevice),
        libc::S_IFIFO => Ok(FileType::Fifo),
        libc::S_IFSOCK => Ok(FileType::Socket),
        _ => Err(ArkError::invalid_argument("mknod file type")),
    }
}

/// True when open flags include O_WRONLY or O_RDWR.
///
/// Used to decide whether an open should go through ro() and whether to
/// allocate a writable InodeBuf. Ignores O_TRUNC for the "writable" bit
/// (O_TRUNC is handled separately).
///
/// Maintainer: O_TRUNC still requires write permission via open_mask.
/// See open() and create_file.
fn is_writable(flags: i32) -> bool {
    let acc = flags & libc::O_ACCMODE;
    acc == libc::O_WRONLY || acc == libc::O_RDWR
}

/// POSIX access mask implied by open flags (`O_TRUNC` requires write).
///
/// Combines R/W according to O_ACCMODE and always adds W when O_TRUNC.
/// Used by open() to call require_mode.
///
/// Maintainer: O_TRUNC implies write even on O_RDONLY base. See open() and
/// "open-file cache is per-inode".
fn open_mask(flags: i32) -> u32 {
    let acc = flags & libc::O_ACCMODE;
    let mut mask = if acc == libc::O_WRONLY {
        ACCESS_W
    } else if acc == libc::O_RDWR {
        ACCESS_R | ACCESS_W
    } else {
        ACCESS_R
    };
    if flags & libc::O_TRUNC != 0 {
        mask |= ACCESS_W;
    }
    mask
}

/// Convert backing `statvfs` into 4096-byte block counts. On failure, return used-only numbers.
///
/// Used by statfs when the real backing filesystem cannot be queried. Returns
/// (blocks, bfree, bavail) scaled to the requested bsize. Never panics.
fn backing_blocks(root: &Path, bsize: u32, used: u64) -> (u64, u64, u64) {
    let Ok(c) = CString::new(root.as_os_str().as_bytes()) else {
        return (used, 0, 0);
    };
    let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut vfs) } != 0 {
        return (used, 0, 0);
    }
    let frag = (vfs.f_frsize as u64).max(1);
    let total = (vfs.f_blocks as u64).saturating_mul(frag);
    let free = (vfs.f_bavail as u64).saturating_mul(frag);
    let b = bsize as u64;
    let blocks = (total / b).max(used);
    let bfree = free / b;
    (blocks, bfree, bfree)
}

/// Convert a TemporalCore FileHandle into a fuser::FileAttr for FUSE replies.
///
/// Root path always projects inode 1 (FUSE_ROOT_ID). Non-root uses
/// attrs.file_id. Size/blocks come from attr_map::to_fuse. crtime uses btime.
/// blksize is fixed at 4096. Never returns 0 inode.
///
/// Maintainer: non-root ino is attrs.file_id. Root is always FUSE_ROOT_ID.
/// See "FUSE root is always inode 1" and handle_to_attr.
pub fn handle_to_attr(h: &FileHandle) -> FileAttr {
    let st = to_fuse(&h.attrs);
    FileAttr {
        ino: if h.path.is_root() {
            FUSE_ROOT_ID
        } else {
            h.attrs.file_id
        },
        size: st.size,
        blocks: st.blocks,
        atime: timespec_to_st(st.atime),
        mtime: timespec_to_st(st.mtime),
        ctime: timespec_to_st(st.ctime),
        crtime: timespec_to_st(h.attrs.btime),
        kind: fuse_kind(h.attrs.file_type),
        perm: (st.mode & 0o7777) as u16,
        nlink: st.nlink,
        uid: st.uid,
        gid: st.gid,
        rdev: st.rdev as u32,
        blksize: 4096,
        flags: 0,
    }
}

/// Canonical [`FileType`] → FUSE kind. `Reparse` projects as a regular file.
pub fn fuse_kind(t: FileType) -> FuseType {
    match t {
        FileType::File | FileType::Reparse => FuseType::RegularFile,
        FileType::Directory => FuseType::Directory,
        FileType::Symlink => FuseType::Symlink,
        FileType::BlockDevice => FuseType::BlockDevice,
        FileType::CharDevice => FuseType::CharDevice,
        FileType::Fifo => FuseType::NamedPipe,
        FileType::Socket => FuseType::Socket,
    }
}

/// Canonical Timespec → SystemTime for fuser::FileAttr.
fn timespec_to_st(t: Timespec) -> SystemTime {
    UNIX_EPOCH + Duration::new(t.sec.max(0) as u64, t.nsec)
}

/// FUSE setattr time: `Now` is wall-clock; `SpecificTime` is the kernel value.
pub fn time_or_now_to_timespec(t: TimeOrNow) -> Timespec {
    match t {
        TimeOrNow::Now => {
            let ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            Timespec::from_nanos(ns)
        }
        TimeOrNow::SpecificTime(st) => {
            let d = st.duration_since(UNIX_EPOCH).unwrap_or_default();
            Timespec::from_nanos(d.as_nanos() as u64)
        }
    }
}

/// [`ArkError`] → FUSE errno. Total match lives in [`crate::err`].
pub fn errno(e: ArkError) -> i32 {
    to_errno(&e)
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;
