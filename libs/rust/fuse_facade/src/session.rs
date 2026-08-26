//! In-process ArkFS node used by FUSE and by unit tests (no `/dev/fuse`).
//!
//! [`ArkSession`] owns a [`temporal_core::TemporalCore`] and a table of open
//! file handles. Writes mutate a whole-object `Vec<u8>` and only hit the store
//! on fsync/flush/release.
//!
//! # Isolation
//!
//! [`ArkSession::mount_store`] opens the data dir with
//! `open_local_quorum_store(..., &["local"])` and `QuorumPolicy::OwnerOnly`.
//! That constructor still builds a `persistent_object_store::LocalQuorum`
//! with an empty remote list (`skip(1)`). Single-node must behave as if no
//! other nodes exist: no `replicas/` directory, no network. Prefer wiring an
//! explicit no-peers backend here when one exists.
//!
//! # Open-file cache traps
//!
//! - Cache is **per fh**, not per inode: two fhs do not share bytes.
//! - `OpenFile.path` is a snapshot; rename/unlink then fsync uses the old path.
//! - `release` currently ignores fsync errors (`let _ = self.fsync(fh)`).
//!
//! Root inode presented to FUSE is always `FUSE_ROOT_ID` (1).

use crate::err::to_errno;
use crate::io_buf::{apply_write, read_slice};
use crate::xattr::{self, SizedBytes};
use arkfs_core::attr_map::{merge_from_fuse, to_fuse, FuseSetAttr};
use arkfs_core::{ArkError, FileType, PathKey, QuorumPolicy, Timespec};
use fuser::{FileAttr, FileType as FuseType, TimeOrNow, FUSE_ROOT_ID};
use persistent_object_store::open_local_quorum_store;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use temporal_core::{DirEntry, FileHandle, TemporalCore, View};

pub const TTL: Duration = Duration::from_secs(1);

/// Kernel fh → cached object. `loaded` is false until first read/write/trunc.
struct OpenFile {
    path: PathKey,
    data: Vec<u8>,
    dirty: bool,
    writable: bool,
    loaded: bool,
}

/// In-process ArkFS node used by FUSE and by unit tests.
pub struct ArkSession {
    pub core: TemporalCore,
    pub view: View,
    pub read_only: bool,
    files: Mutex<HashMap<u64, OpenFile>>,
    next_fh: Mutex<u64>,
}

impl ArkSession {
    /// Open `--data DIR`. `as_of_logical: Some(n)` is a read-only historical mount.
    pub fn mount_store(data_dir: &Path, as_of_logical: Option<u64>) -> Result<Self, ArkError> {
        let store = open_local_quorum_store(data_dir, &["local"])?;
        let core = TemporalCore::open(store, QuorumPolicy::OwnerOnly)?;
        core.ensure_root()?;
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
            files: Mutex::new(HashMap::new()),
            next_fh: Mutex::new(1),
        })
    }

    fn ro(&self) -> Result<(), ArkError> {
        if self.read_only {
            Err(ArkError::ReadOnly)
        } else {
            Ok(())
        }
    }

    /// Lookup `name` in FUSE parent inode using the session's live/as-of view.
    pub fn lookup(&self, parent: u64, name: &str) -> Result<FileHandle, ArkError> {
        let parent = self.core.lookup_ino(parent, self.view)?;
        let path = parent.path.join(name)?;
        self.core.lookup(path.as_str(), self.view)
    }

    /// Stat. If `fh` is a loaded open file, `size` comes from the cache (dirty writes).
    pub fn getattr(&self, ino: u64, fh: Option<u64>) -> Result<FileAttr, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        let mut attr = handle_to_attr(&h);
        if let Some(fh) = fh {
            let files = self.files.lock().unwrap();
            if let Some(of) = files.get(&fh) {
                if of.loaded {
                    attr.size = of.data.len() as u64;
                    attr.blocks = attr.size.div_ceil(512);
                }
            }
        }
        Ok(attr)
    }

    /// Partial setattr. Size may hit the open cache; other fields go through attr_map.
    pub fn setattr(
        &self,
        ino: u64,
        patch: FuseSetAttr,
        fh: Option<u64>,
    ) -> Result<FileAttr, ArkError> {
        self.ro()?;
        let h = self.core.lookup_ino(ino, View::Live)?;
        if let Some(size) = patch.size {
            if let Some(fh) = fh {
                self.truncate_open(fh, size)?;
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

    /// Create a directory under FUSE parent inode. Fails if the session is as-of RO.
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
        self.core.mkdir(parent.path.as_str(), name, mode, uid, gid)
    }

    /// FUSE create: mkdir-like file create plus an open fh (often with `O_RDWR`).
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
        let h = self
            .core
            .create_file(parent.path.as_str(), name, mode & 0o7777, uid, gid)?;
        let fh = self.open_handle(h.path.clone(), flags, true)?;
        Ok((h, fh))
    }

    pub fn unlink(&self, parent: u64, name: &str) -> Result<(), ArkError> {
        self.ro()?;
        let path = self.child_path(parent, name)?;
        self.core.unlink(path.as_str())
    }

    pub fn rmdir(&self, parent: u64, name: &str) -> Result<(), ArkError> {
        self.ro()?;
        let path = self.child_path(parent, name)?;
        self.core.rmdir(path.as_str())
    }

    pub fn rename(
        &self,
        parent: u64,
        name: &str,
        newparent: u64,
        newname: &str,
    ) -> Result<(), ArkError> {
        self.ro()?;
        let from = self.child_path(parent, name)?;
        let to_parent = self.core.lookup_ino(newparent, View::Live)?;
        self.core
            .rename(from.as_str(), to_parent.path.as_str(), newname)?;
        Ok(())
    }

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
        self.core
            .symlink(parent.path.as_str(), name, target, uid, gid)
    }

    pub fn readlink(&self, ino: u64) -> Result<Vec<u8>, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        match h.attrs.symlink_target {
            Some(t) => Ok(t.into_bytes()),
            None => self.core.read_content(&h),
        }
    }

    pub fn readdir(&self, ino: u64) -> Result<Vec<DirEntry>, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        self.core.readdir(h.path.as_str(), self.view)
    }

    /// Open existing inode. Directories are EISDIR. Write flags require a live mount.
    pub fn open(&self, ino: u64, flags: i32) -> Result<u64, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        if h.attrs.file_type == FileType::Directory {
            return Err(ArkError::is_a_directory(h.path.as_str()));
        }
        let writable = is_writable(flags);
        if writable {
            self.ro()?;
        }
        self.open_handle(h.path, flags, writable)
    }

    pub fn read(&self, fh: u64, offset: i64, size: u32) -> Result<Vec<u8>, ArkError> {
        let mut files = self.files.lock().unwrap();
        let of = files
            .get_mut(&fh)
            .ok_or_else(|| ArkError::invalid_argument("bad fh"))?;
        self.ensure_loaded(of)?;
        Ok(read_slice(&of.data, offset, size).to_vec())
    }

    pub fn write(&self, fh: u64, offset: i64, data: &[u8]) -> Result<u32, ArkError> {
        self.ro()?;
        let mut files = self.files.lock().unwrap();
        let of = files
            .get_mut(&fh)
            .ok_or_else(|| ArkError::invalid_argument("bad fh"))?;
        if !of.writable {
            return Err(ArkError::ReadOnly);
        }
        self.ensure_loaded(of)?;
        apply_write(&mut of.data, offset, data);
        of.dirty = true;
        Ok(data.len() as u32)
    }

    /// Persist dirty fh bytes via `replace_content`. No-op if not dirty.
    pub fn fsync(&self, fh: u64) -> Result<(), ArkError> {
        let (path, data) = {
            let mut files = self.files.lock().unwrap();
            let of = files
                .get_mut(&fh)
                .ok_or_else(|| ArkError::invalid_argument("bad fh"))?;
            if !of.dirty {
                return Ok(());
            }
            (of.path.clone(), of.data.clone())
        };
        self.core.replace_content(path.as_str(), data)?;
        let mut files = self.files.lock().unwrap();
        if let Some(of) = files.get_mut(&fh) {
            of.dirty = false;
        }
        Ok(())
    }

    /// Flush dirty bytes then drop the fh. Fsync errors are currently swallowed.
    pub fn release(&self, fh: u64) -> Result<(), ArkError> {
        let _ = self.fsync(fh);
        self.files.lock().unwrap().remove(&fh);
        Ok(())
    }

    pub fn setxattr(&self, ino: u64, name: &str, value: &[u8]) -> Result<(), ArkError> {
        self.ro()?;
        let h = self.core.lookup_ino(ino, View::Live)?;
        self.core.commit_attrs_now(h.path.as_str(), |attrs| {
            attrs.xattrs.insert(name.to_string(), value.to_vec());
        })?;
        Ok(())
    }

    pub fn getxattr(&self, ino: u64, name: &str) -> Result<Vec<u8>, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        h.attrs
            .xattrs
            .get(name)
            .cloned()
            .ok_or_else(|| ArkError::not_found(name))
    }

    pub fn getxattr_sized(&self, ino: u64, name: &str, size: u32) -> Result<SizedBytes, ArkError> {
        Ok(xattr::sized(&self.getxattr(ino, name)?, size))
    }

    pub fn listxattr(&self, ino: u64) -> Result<Vec<u8>, ArkError> {
        let h = self.core.lookup_ino(ino, self.view)?;
        Ok(xattr::encode_list(
            h.attrs.xattrs.keys().map(String::as_str),
        ))
    }

    pub fn listxattr_sized(&self, ino: u64, size: u32) -> Result<SizedBytes, ArkError> {
        Ok(xattr::sized(&self.listxattr(ino)?, size))
    }

    pub fn removexattr(&self, ino: u64, name: &str) -> Result<(), ArkError> {
        self.ro()?;
        let h = self.core.lookup_ino(ino, View::Live)?;
        self.core.commit_attrs_now(h.path.as_str(), |attrs| {
            attrs.xattrs.remove(name);
        })?;
        Ok(())
    }

    /// Create a regular file without opening it (FUSE mknod).
    pub fn mknod_regular(
        &self,
        parent: u64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<FileHandle, ArkError> {
        self.ro()?;
        let parent = self.core.lookup_ino(parent, View::Live)?;
        self.core
            .create_file(parent.path.as_str(), name, mode & 0o7777, uid, gid)
    }

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
            )?;
        }
        Ok(())
    }

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

    pub fn object_count(&self) -> Result<u64, ArkError> {
        Ok(self.core.store().verify_integrity()?.objects_checked)
    }

    fn child_path(&self, parent: u64, name: &str) -> Result<PathKey, ArkError> {
        let parent = self.core.lookup_ino(parent, self.view)?;
        parent.path.join(name)
    }

    fn open_handle(&self, path: PathKey, flags: i32, writable: bool) -> Result<u64, ArkError> {
        let mut of = OpenFile {
            path,
            data: Vec::new(),
            dirty: false,
            writable,
            loaded: false,
        };
        if writable && (flags & libc::O_TRUNC) != 0 {
            of.data.clear();
            of.loaded = true;
            of.dirty = true;
        }
        let mut next = self.next_fh.lock().unwrap();
        let fh = *next;
        *next += 1;
        self.files.lock().unwrap().insert(fh, of);
        Ok(fh)
    }

    fn ensure_loaded(&self, of: &mut OpenFile) -> Result<(), ArkError> {
        if of.loaded {
            return Ok(());
        }
        let h = self.core.lookup(of.path.as_str(), self.view)?;
        of.data = self.core.read_content(&h)?;
        of.loaded = true;
        Ok(())
    }

    fn truncate_open(&self, fh: u64, size: u64) -> Result<(), ArkError> {
        let mut files = self.files.lock().unwrap();
        let of = files
            .get_mut(&fh)
            .ok_or_else(|| ArkError::invalid_argument("bad fh"))?;
        self.ensure_loaded(of)?;
        of.data.resize(size as usize, 0);
        of.dirty = true;
        Ok(())
    }
}

fn is_writable(flags: i32) -> bool {
    let acc = flags & libc::O_ACCMODE;
    acc == libc::O_WRONLY || acc == libc::O_RDWR
}

/// Temporal handle → `fuser::FileAttr`. Non-root `ino` is `attrs.file_id`.
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

/// Map canonical [`FileType`] to FUSE. `Reparse` becomes a regular file.
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

fn timespec_to_st(t: Timespec) -> SystemTime {
    UNIX_EPOCH + Duration::new(t.sec.max(0) as u64, t.nsec)
}

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

pub fn errno(e: ArkError) -> i32 {
    to_errno(&e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sess() -> (tempfile::TempDir, ArkSession) {
        let d = tempdir().unwrap();
        let s = ArkSession::mount_store(d.path(), None).unwrap();
        (d, s)
    }

    #[test]
    fn mkdir_create_write_read_unlink() {
        let (_d, s) = sess();
        s.mkdir(FUSE_ROOT_ID, "w", 0o755, 0, 0).unwrap();
        let dir = s.lookup(FUSE_ROOT_ID, "w").unwrap();
        let (f, fh) = s
            .create_file(dir.attrs.file_id, "a.txt", 0o644, 0, 0, libc::O_RDWR)
            .unwrap();
        assert_eq!(s.write(fh, 0, b"abc").unwrap(), 3);
        s.fsync(fh).unwrap();
        s.release(fh).unwrap();
        let fh = s.open(f.attrs.file_id, libc::O_RDONLY).unwrap();
        assert_eq!(s.read(fh, 0, 10).unwrap(), b"abc");
        s.release(fh).unwrap();
        s.unlink(dir.attrs.file_id, "a.txt").unwrap();
        assert!(s.lookup(dir.attrs.file_id, "a.txt").is_err());
        let names: Vec<_> = s
            .readdir(dir.attrs.file_id)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.is_empty());
    }

    #[test]
    fn as_of_is_read_only() {
        let d = tempdir().unwrap();
        let s = ArkSession::mount_store(d.path(), None).unwrap();
        s.mkdir(FUSE_ROOT_ID, "d", 0o755, 0, 0).unwrap();
        drop(s);
        let s = ArkSession::mount_store(d.path(), Some(1)).unwrap();
        assert!(s.mkdir(FUSE_ROOT_ID, "nope", 0o755, 0, 0).is_err());
    }

    #[test]
    fn setattr_mode() {
        let (_d, s) = sess();
        let (f, fh) = s
            .create_file(FUSE_ROOT_ID, "m", 0o644, 0, 0, libc::O_RDWR)
            .unwrap();
        s.release(fh).unwrap();
        s.setattr(
            f.attrs.file_id,
            FuseSetAttr {
                mode: Some(0o600),
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let a = s.getattr(f.attrs.file_id, None).unwrap();
        assert_eq!(a.perm, 0o600);
    }
}
