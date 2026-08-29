//! `fuser::Filesystem` adapter. Keep this file a translator.
//!
//! Each method: parse UTF-8 name → call [`ArkSession`] → `reply.*` or errno.
//! Do not implement mkdir/unlink semantics here.
//!
//! `mount` and `spawn` share the same option set (no AutoUnmount: that implies
//! allow_other on stock fuse.conf). Unmount with `arkfs umount` / fusermount3.
//! POSIX locks, poll, bmap, copy_file_range, and readdirplus are implemented
//! in userspace (`FUSE_POSIX_LOCKS` / `FUSE_DO_READDIRPLUS` advertised in
//! `init`). SETLKW replies from a helper thread so the session loop can still
//! process the matching unlock.
//!
//! Maintainer: this layer must stay a thin adapter. All POSIX and temporal
//! semantics live in ArkSession / TemporalCore. See "Do not put filesystem
//! logic in FuseFs" in maintainer.md.

use crate::session::{errno, fuse_kind, handle_to_attr, time_or_now_to_timespec, ArkSession, TTL};
use crate::xattr::SizedBytes;
use arkfs_core::PosixPatch;
use fuser::{
    consts, FileType as FuseType, Filesystem, KernelConfig, MountOption, PollHandle, ReplyAttr,
    ReplyBmap, ReplyCreate, ReplyData, ReplyDirectory, ReplyDirectoryPlus, ReplyEmpty, ReplyEntry,
    ReplyIoctl, ReplyLock, ReplyLseek, ReplyOpen, ReplyPoll, ReplyStatfs, ReplyWrite, ReplyXattr,
    Request, TimeOrNow,
};
use libc::{c_int, ENOTTY, ERANGE};
use std::ffi::OsStr;
use std::path::Path;
use std::thread;
use std::time::SystemTime;

/// Newtype over [`ArkSession`] so `Filesystem` impl stays in this module.
///
/// Maintainer: thin adapter only. See "Do not put filesystem logic in FuseFs".
pub struct FuseFs(pub ArkSession);

/// UTF-8 FUSE names only. Non-UTF8 is EINVAL (same as the kernel-facing adapter).
///
/// Returns the `&str` on success. On failure returns `EINVAL`.
/// Used by every name-bearing FUSE op before delegating to ArkSession.
///
/// Maintainer: names must be valid UTF-8 per FUSE contract; see "Names must be UTF-8".
pub fn fuse_name(name: &OsStr) -> Result<&str, i32> {
    name.to_str().ok_or(libc::EINVAL)
}

/// FUSE xattr reply: size / data / ERANGE. Missing getxattr is ENODATA.
///
/// On `SizedBytes::Size` replies the length. On `Data` sends bytes.
/// On `Range` replies ERANGE. On NotFound and `missing_is_nodata` uses ENODATA,
/// otherwise maps via `errno`.
///
/// Preconditions: name already validated UTF-8 by caller.
/// Postconditions: reply is always completed exactly once.
///
/// Maintainer: xattr size/data protocol is FUSE-specific; see xattr.rs and
/// "XATTR_CREATE / XATTR_REPLACE".
fn reply_sized(
    reply: ReplyXattr,
    r: Result<SizedBytes, arkfs_core::ArkError>,
    missing_is_nodata: bool,
) {
    match r {
        Ok(SizedBytes::Size(n)) => reply.size(n),
        Ok(SizedBytes::Data(v)) => reply.data(&v),
        Ok(SizedBytes::Range) => reply.error(ERANGE),
        Err(e) => {
            if missing_is_nodata && matches!(e, arkfs_core::ArkError::NotFound { .. }) {
                reply.error(libc::ENODATA);
            } else {
                reply.error(errno(e));
            }
        }
    }
}

impl Filesystem for FuseFs {
    /// Advertise POSIX locks, atomic O_TRUNC, and readdirplus. Unknown caps are skipped.
    ///
    /// Adds FUSE_POSIX_LOCKS, FUSE_ATOMIC_O_TRUNC, FUSE_FLOCK_LOCKS,
    /// FUSE_DO_READDIRPLUS, FUSE_READDIRPLUS_AUTO. Failures to add a cap are
    /// ignored (kernel may not support it). No other side effects.
    ///
    /// Preconditions: called once at mount time before other ops.
    /// Postconditions: advertised caps are visible to the kernel for the mount.
    ///
    /// Maintainer: userspace POSIX locks and readdirplus are implemented here;
    /// see "POSIX locks" and "How to add a FUSE operation" in maintainer.md.
    fn init(&mut self, _req: &Request<'_>, config: &mut KernelConfig) -> Result<(), c_int> {
        for cap in [
            consts::FUSE_POSIX_LOCKS,
            consts::FUSE_ATOMIC_O_TRUNC,
            consts::FUSE_FLOCK_LOCKS,
            consts::FUSE_DO_READDIRPLUS,
            consts::FUSE_READDIRPLUS_AUTO,
        ] {
            let _ = config.add_capabilities(cap);
        }
        Ok(())
    }

    /// Unmount teardown. Retry dirty inodes that failed persist on release.
    ///
    /// Best-effort flush of every dirty inode buffer. Errors are swallowed
    /// (the mount is going away). Called by fuser after the last FUSE request.
    ///
    /// Preconditions: no new FUSE ops will arrive after destroy starts.
    /// Postconditions: every InodeBuf that was dirty has had one persist attempt.
    ///
    /// Maintainer: see FuseFs::destroy + ArkSession::flush_dirty and
    /// "destroy" / "fsync / flush / release must persist".
    fn destroy(&mut self) {
        let _ = self.0.flush_dirty();
    }

    /// Kernel dropped a lookup ref. Inodes are durable file_ids; we do not evict.
    ///
    /// No-op. Inode numbers are stable file_ids from the temporal index.
    /// We keep no reference-counted inode cache to evict.
    ///
    /// Preconditions: ino was previously looked up or opened.
    /// Postconditions: nothing changes in the session.
    ///
    /// Maintainer: "Inodes are durable file_ids; we do not evict." See
    /// lookup_ino and "Open-file cache is per-inode".
    fn forget(&mut self, _req: &Request<'_>, _ino: u64, _nlookup: u64) {}

    /// Lookup a name under parent inode and return a directory entry.
    ///
    /// Validates UTF-8. Calls ArkSession::lookup which does X_OK on parent and
    /// hides tombstones. On success replies entry + generation. Non-UTF8 or
    /// permission error maps to EINVAL / EACCES via errno.
    ///
    /// Preconditions: parent is a directory inode visible in the session view.
    /// Postconditions: kernel has a positive lookup count for the child inode.
    ///
    /// Maintainer: permission check (X_OK) is in ArkSession; see "lookup" and
    /// "Unix permission bits". Thin adapter only.
    fn lookup(&mut self, req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let Ok(name) = fuse_name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.lookup(parent, name, req.uid(), req.gid()) {
            Ok(h) => reply.entry(&TTL, &handle_to_attr(&h), h.attrs.generation),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Return attributes for inode (or open fh).
    ///
    /// Passes optional fh to ArkSession so that dirty size from the per-inode
    /// InodeBuf is visible (fstat on open file after unlink). On success
    /// replies attr with TTL. Errors are mapped via errno.
    ///
    /// Preconditions: ino (or fh) refers to a live or unlinked-but-open inode.
    /// Postconditions: returned attr reflects last persisted state or current
    /// dirty buffer size.
    ///
    /// Maintainer: dirty size from the per-inode buffer must be visible to
    /// fstat on open fhs even after the name is unlinked. See getattr +
    /// overlay_cached_size and "Open-file cache is per-inode".
    fn getattr(&mut self, _req: &Request<'_>, ino: u64, fh: Option<u64>, reply: ReplyAttr) {
        match self.0.getattr(ino, fh) {
            Ok(a) => reply.attr(&TTL, &a),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Partial POSIX setattr (mode/uid/gid/size/times). Unset fields stay put.
    ///
    /// Builds a PosixPatch and calls ArkSession::setattr. Owner/root checks,
    /// merge_from_fuse, and size handling via inode buffer or truncate are done
    /// in the session. Replies the resulting attr or errno.
    ///
    /// Preconditions: caller has a valid inode (fh optional for size).
    /// Postconditions: only listed fields change; others preserved via merge.
    ///
    /// Maintainer: delegates to ArkSession::setattr which must use
    /// merge_from_fuse (Partial setattr trap). Size path may truncate the
    /// shared inode buffer. See "Partial setattr".
    fn setattr(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        ctime: Option<SystemTime>,
        fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let patch = PosixPatch {
            mode,
            uid,
            gid,
            size,
            atime: atime.map(time_or_now_to_timespec),
            mtime: mtime.map(time_or_now_to_timespec),
            ctime: ctime.map(|st| time_or_now_to_timespec(TimeOrNow::SpecificTime(st))),
        };
        match self.0.setattr(ino, patch, fh, req.uid(), req.gid()) {
            Ok(a) => reply.attr(&TTL, &a),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Create a non-directory node (regular, fifo, socket, device).
    ///
    /// Validates UTF-8 name. Strips umask from mode. Delegates to
    /// ArkSession::mknod which does parent W+X, inherit, commit_branch (fresh
    /// file_id if needed), and safe publish. Replies entry on success.
    ///
    /// Preconditions: parent is a directory; name is free or a tombstone.
    /// Postconditions: new inode exists with given type and mode bits.
    ///
    /// Maintainer: delegates to core mknod / commit_branch (Inode 0 trap +
    /// conflict-before-write). See "mknod".
    fn mknod(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        rdev: u32,
        reply: ReplyEntry,
    ) {
        let Ok(name) = fuse_name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self
            .0
            .mknod(parent, name, mode & !umask, req.uid(), req.gid(), rdev)
        {
            Ok(h) => reply.entry(&TTL, &handle_to_attr(&h), h.attrs.generation),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Create a directory under parent.
    ///
    /// Validates UTF-8. Applies umask. Calls ArkSession::mkdir (parent W+X,
    /// inherit setgid, commit_branch with fresh file_id). Replies entry.
    ///
    /// Preconditions: parent directory, name free or tombstoned.
    /// Postconditions: new directory inode appears in live view.
    ///
    /// Maintainer: mkdir path goes through commit_branch (Inode 0 trap) and
    /// inherit_from_parent. See "mkdir" and "setgid directories".
    fn mkdir(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let Ok(name) = fuse_name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self
            .0
            .mkdir(parent, name, mode & !umask, req.uid(), req.gid())
        {
            Ok(h) => reply.entry(&TTL, &handle_to_attr(&h), h.attrs.generation),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Tombstone a non-directory name (unlink).
    ///
    /// Validates UTF-8. Calls ArkSession::unlink which flushes the target inode
    /// first (so dirty data becomes a version), then appends a tombstone via
    /// TemporalCore. Replies ok or errno. Sticky-bit and ownership checks are
    /// in the session.
    ///
    /// Preconditions: name exists as non-dir in live view; caller has W+X on parent.
    /// Postconditions: name is a tombstone in the next index; bytes remain.
    ///
    /// Maintainer: must flush before tombstone (flush before unlink). Never-delete:
    /// unlink appends a tombstone, does not remove objects. See "flush before unlink"
    /// and "Never-delete in practice".
    fn unlink(&mut self, req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let Ok(name) = fuse_name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.unlink(parent, name, req.uid(), req.gid()) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Tombstone an empty directory (rmdir).
    ///
    /// Validates UTF-8. Calls ArkSession::rmdir. Emptiness is checked at
    /// tombstone time in the core. Root cannot be removed. Replies ok or errno.
    ///
    /// Preconditions: name is an empty dir visible in live view; W+X on parent.
    /// Postconditions: name is tombstoned; directory inode may still be open.
    ///
    /// Maintainer: rmdir is just a tombstone (never-delete). Emptiness checked
    /// at tombstone time. See "rmdir".
    fn rmdir(&mut self, req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let Ok(name) = fuse_name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.rmdir(parent, name, req.uid(), req.gid()) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Create a symlink under parent. Target must be valid UTF-8 path.
    ///
    /// Validates both link_name and target. Calls ArkSession::symlink which
    /// stores target both in attrs.symlink_target and as content object.
    /// Replies the new entry.
    ///
    /// Preconditions: parent dir writable; link_name free.
    /// Postconditions: symlink inode exists with target recorded.
    ///
    /// Maintainer: target bytes written in two places by TemporalCore::symlink
    /// for readlink fallback. See "symlink".
    fn symlink(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        link_name: &OsStr,
        target: &std::path::Path,
        reply: ReplyEntry,
    ) {
        let Ok(name) = fuse_name(link_name) else {
            reply.error(libc::EINVAL);
            return;
        };
        let Some(target) = target.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.symlink(parent, name, target, req.uid(), req.gid()) {
            Ok(h) => reply.entry(&TTL, &handle_to_attr(&h), h.attrs.generation),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Return symlink target bytes.
    ///
    /// Delegates to ArkSession::readlink. Prefers attrs.symlink_target,
    /// falls back to content bytes. Replies data or errno.
    ///
    /// Preconditions: ino is a symlink in the session view.
    /// Postconditions: kernel receives the exact target bytes.
    ///
    /// Maintainer: both attr and content locations are written by core symlink
    /// so readlink works even if projection changes. See "symlink".
    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.0.readlink(ino) {
            Ok(b) => reply.data(&b),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Create a hard link: add an extra name for existing inode under newparent.
    ///
    /// Validates UTF-8 newname. Calls ArkSession::link which flushes source
    /// inode first, then adds the extra directory entry sharing the same file_id.
    /// Replies the entry for the new name.
    ///
    /// Preconditions: source ino is live non-dir; newparent is dir; newname free.
    /// Postconditions: two names resolve to the same inode / file_id.
    ///
    /// Maintainer: must flush source first so dirty data becomes history before
    /// the link. Hard links share file_id and thus the open inode buffer.
    /// See "link" and "Open-file cache is per-inode".
    fn link(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        let Ok(newname) = fuse_name(newname) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.link(ino, newparent, newname, req.uid(), req.gid()) {
            Ok(h) => reply.entry(&TTL, &handle_to_attr(&h), h.attrs.generation),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Move/rename a name. flags are Linux renameat2 bits (0, RENAME_EXCHANGE, etc.).
    ///
    /// Validates UTF-8 for both names. Delegates to ArkSession::rename which
    /// flushes source, calls core rename (rebase for dirs + tombstone fan-out),
    /// then syncs open inode paths. Replies ok or errno.
    ///
    /// Preconditions: source exists; destination rules per flags; W+X on both dirs.
    /// Postconditions: name moved (or exchanged); all live descendants updated in one persist.
    ///
    /// Maintainer: directory rename retargets every live descendant via
    /// PathKey::rebase in one index persist (Directory rename trap). Must also
    /// flush_under and sync_inode_paths. See "Directory rename".
    fn rename(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        let (Ok(name), Ok(newname)) = (fuse_name(name), fuse_name(newname)) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.rename(
            parent,
            name,
            newparent,
            newname,
            flags,
            req.uid(),
            req.gid(),
        ) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Open a non-directory inode; returns a file handle for the shared inode buffer.
    ///
    /// Calls ArkSession::open. For writable opens the session checks ro() and
    /// require_mode. O_TRUNC is applied to the per-inode buffer at open time.
    /// fh is returned; flags are not passed to the kernel reply (0).
    ///
    /// Preconditions: ino is regular (or special file that allows open); permissions ok.
    /// Postconditions: fh is valid until release; all fhs on inode share one InodeBuf.
    ///
    /// Maintainer: returns a per-inode fh. Writable opens go through ro().
    /// See "open-file cache is per-inode" and release/flush.
    fn open(&mut self, req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        match self.0.open(ino, flags, req.uid(), req.gid()) {
            Ok(fh) => reply.opened(fh, 0),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Read from the open inode buffer (regular file data).
    ///
    /// Delegates to ArkSession::read. Negative offset treated as 0. Size is
    /// capped by the buffer contents. Returns bytes or errno (EBADF etc.).
    ///
    /// Preconditions: fh is valid for a readable open.
    /// Postconditions: atime may be updated (best-effort relatime).
    ///
    /// Maintainer: content comes from the per-inode InodeBuf. See "read" and
    /// "maybe_touch_atime". Thin adapter.
    fn read(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        match self.0.read(fh, offset, size) {
            Ok(b) => reply.data(&b),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Write into the open inode buffer (regular files only).
    ///
    /// Delegates to ArkSession::write which mutates the shared per-inode
    /// InodeBuf, marks dirty, and may extend the buffer. Returns bytes written
    /// or errno. Persistence occurs later on fsync/flush/release.
    ///
    /// Preconditions: fh is writable; offset >= 0; len fits in u32 and <= 1 GiB.
    /// Postconditions: buffer is dirty; size visible via getattr(fh).
    ///
    /// Maintainer: writes are buffered per-inode. All fhs on the same inode
    /// share the buffer. Persistence happens on fsync/release. See "Open-file
    /// cache is per-inode" trap and flush_ino/release.
    fn write(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        match self.0.write(fh, offset, data) {
            Ok(n) => reply.written(n),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// FUSE flush (close of an fd): persist bytes and drop POSIX locks for lock_owner.
    ///
    /// Calls ArkSession::flush which unlocks for the owner then flush_ino.
    /// Replies ok or errno. This is per-fd close(2), not the final release.
    ///
    /// Preconditions: fh valid.
    /// Postconditions: dirty bytes for the inode are durable (or error); locks for owner dropped.
    ///
    /// Maintainer: called on every close(2) of an fd. Must persist. Locks for
    /// the lock_owner are dropped here. See "flush" and "POSIX locks".
    fn flush(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        match self.0.flush(fh, lock_owner) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Persist dirty bytes for the inode of fh. Does not drop locks.
    ///
    /// Delegates to ArkSession::fsync which writes the whole current InodeBuf
    /// through replace_content + commit_branch + persist_index. datasync is
    /// accepted but ignored (whole-object store).
    ///
    /// Preconditions: fh valid.
    /// Postconditions: on success the bytes are a new version in the index.
    ///
    /// Maintainer: open-file cache is per-inode. fsync must persist the whole
    /// buffer before returning. On error the buffer must remain dirty.
    /// See "fsync / flush / release must persist" and "Open-file cache is per-inode".
    fn fsync(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.0.fsync(fh) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Last close of fh: persist dirty bytes, drop locks for owner if given, drop the handle.
    ///
    /// Calls unlock_owner if lock_owner present, then ArkSession::release which
    /// attempts flush_ino and drops the fh refcount. On persist failure the
    /// buffer stays and error is returned.
    ///
    /// Preconditions: fh was returned by a prior open.
    /// Postconditions: fh is invalid; inode buffer refcount decreased.
    ///
    /// Maintainer: release must attempt to persist. If persist fails, keep the
    /// buffer (refs, dirty) and return the error. This is the critical "release
    /// must persist before dropping the fh" durability rule.
    /// See "release must persist before dropping the fh" trap and
    /// "Open-file cache is per-inode".
    fn release(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        _flags: i32,
        lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        if let Some(owner) = lock_owner {
            self.0.unlock_owner(ino, owner);
        }
        match self.0.release(fh) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Open a directory. Must be a directory; search permission (X_OK) required.
    ///
    /// Delegates to ArkSession::opendir. fh returned is a dummy (0) because
    /// directories are stateless. Permission enforced in session.
    ///
    /// Preconditions: ino is a directory in the view; caller has X_OK.
    /// Postconditions: fh is valid for readdir/releasedir until closed.
    ///
    /// Maintainer: directories are stateless (fh is a dummy 0). Permission
    /// is X_OK only. See "opendir".
    fn opendir(&mut self, req: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        match self.0.opendir(ino, req.uid(), req.gid()) {
            Ok(fh) => reply.opened(fh, 0),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// List directory children plus `.` and `..`. Offset is the next entry index.
    ///
    /// Obtains parent_ino and readdir entries from ArkSession (which enforces
    /// R+X). Builds the synthetic . / .. then appends children. Uses reply.add
    /// and stops on full buffer. Offset is entry index (not cookie).
    ///
    /// Preconditions: ino is a directory the caller can read.
    /// Postconditions: kernel receives a complete or partial directory listing.
    ///
    /// Maintainer: delegates to TemporalCore::readdir which is a path-prefix
    /// scan (not inode children). See "readdir is a prefix scan".
    fn readdir(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let parent_ino = match self.0.parent_ino(ino) {
            Ok(p) => p,
            Err(e) => {
                reply.error(errno(e));
                return;
            }
        };
        let mut entries: Vec<(u64, FuseType, String)> = vec![
            (ino, FuseType::Directory, ".".into()),
            (parent_ino, FuseType::Directory, "..".into()),
        ];
        match self.0.readdir(ino, req.uid(), req.gid()) {
            Ok(dir) => {
                for e in dir {
                    entries.push((e.ino, fuse_kind(e.file_type), e.name));
                }
            }
            Err(e) => {
                reply.error(errno(e));
                return;
            }
        }
        for (i, (ino, kind, name)) in entries.into_iter().enumerate().skip(offset as usize) {
            let next = (i + 1) as i64;
            if reply.add(ino, next, kind, name) {
                break;
            }
        }
        reply.ok();
    }

    /// readdirplus: directory listing with attributes (LOOKUP packed in).
    ///
    /// Same structure as readdir but each entry carries full FileAttr and
    /// generation. Builds . and .. using getattr. Delegates children to
    /// ArkSession::readdir_plus. Uses reply.add with TTL.
    ///
    /// Preconditions: ino is a readable directory.
    /// Postconditions: kernel receives attrs so it can avoid separate lookups.
    ///
    /// Maintainer: still a thin translator. Real readdir semantics in core.
    /// See "readdir is a prefix scan".
    fn readdirplus(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectoryPlus,
    ) {
        let self_attr = match self.0.getattr(ino, None) {
            Ok(a) => a,
            Err(e) => {
                reply.error(errno(e));
                return;
            }
        };
        let parent_ino = match self.0.parent_ino(ino) {
            Ok(p) => p,
            Err(e) => {
                reply.error(errno(e));
                return;
            }
        };
        let parent_attr = if parent_ino == ino {
            self_attr
        } else {
            match self.0.getattr(parent_ino, None) {
                Ok(a) => a,
                Err(e) => {
                    reply.error(errno(e));
                    return;
                }
            }
        };
        let mut entries: Vec<(u64, String, fuser::FileAttr, u64)> = vec![
            (ino, ".".into(), self_attr, 0),
            (parent_ino, "..".into(), parent_attr, 0),
        ];
        match self.0.readdir_plus(ino, req.uid(), req.gid()) {
            Ok(dir) => {
                for (e, attr, gen) in dir {
                    entries.push((e.ino, e.name, attr, gen));
                }
            }
            Err(e) => {
                reply.error(errno(e));
                return;
            }
        }
        for (i, (ino, name, attr, gen)) in entries.into_iter().enumerate().skip(offset as usize) {
            let next = (i + 1) as i64;
            if reply.add(ino, next, name, &TTL, &attr, gen) {
                break;
            }
        }
        reply.ok();
    }

    /// Close a directory handle. Nothing to persist.
    ///
    /// Directories are stateless; this is a no-op that just replies ok.
    /// fh is ignored.
    ///
    /// Preconditions: fh was returned by opendir on the same ino.
    /// Postconditions: no state change.
    ///
    /// Maintainer: directories are stateless (fh is a dummy 0). See "opendir".
    fn releasedir(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _flags: i32,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    /// Create a regular file and open it atomically (FUSE create).
    ///
    /// Validates UTF-8. Strips umask. Delegates to ArkSession::create_file
    /// which does parent W+X, commit_branch for the new inode, opens the
    /// per-inode buffer, and returns (handle, fh). Replies created + fh.
    ///
    /// Preconditions: parent dir; name free or tombstoned; flags allow write.
    /// Postconditions: file exists and is open; fh shares the inode buffer.
    ///
    /// Maintainer: create_file path must allocate fresh file_id (Inode 0 trap)
    /// and open the shared InodeBuf. See "create" and "Open-file cache is per-inode".
    fn create(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let Ok(name) = fuse_name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self
            .0
            .create_file(parent, name, mode & !umask, req.uid(), req.gid(), flags)
        {
            Ok((h, fh)) => reply.created(&TTL, &handle_to_attr(&h), h.attrs.generation, fh, 0),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Set one xattr (UTF-8 name). Supports XATTR_CREATE / REPLACE.
    ///
    /// Validates UTF-8 name. Delegates to ArkSession::setxattr which checks W,
    /// enforces create/replace flags, and commits via commit_attrs (fans out
    /// over hard links). Value may be any bytes.
    ///
    /// Preconditions: ino visible; caller has W; flags valid.
    /// Postconditions: xattr present on all names of the inode.
    ///
    /// Maintainer: uses commit_attrs which fans out via hard links. Respects
    /// XATTR_CREATE / REPLACE. See "xattr" and "XATTR_CREATE / XATTR_REPLACE".
    fn setxattr(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        value: &[u8],
        flags: i32,
        _position: u32,
        reply: ReplyEmpty,
    ) {
        let Ok(name) = fuse_name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self
            .0
            .setxattr(ino, name, value, flags, req.uid(), req.gid())
        {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Get one xattr. Missing name returns ENODATA.
    ///
    /// Validates UTF-8. Uses reply_sized with missing_is_nodata=true so NotFound
    /// becomes ENODATA. Size==0 returns length; otherwise data or ERANGE.
    ///
    /// Preconditions: ino visible.
    /// Postconditions: reply completed with size, data, ERANGE or ENODATA.
    ///
    /// Maintainer: xattr size/data protocol handled in reply_sized; core
    /// returns NotFound for missing. See xattr.rs.
    fn getxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        let Ok(name) = fuse_name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        reply_sized(reply, self.0.getxattr_sized(ino, name, size), true);
    }

    /// List xattr names for inode (NUL-separated payload for FUSE).
    ///
    /// Uses reply_sized with missing_is_nodata=false (empty list is valid).
    /// Size==0 returns total length; otherwise the NUL-separated bytes or ERANGE.
    ///
    /// Preconditions: ino visible.
    /// Postconditions: reply completed.
    ///
    /// Maintainer: same sized protocol as getxattr. Core listxattr walks
    /// attrs.xattrs. See xattr.rs and "xattr".
    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        reply_sized(reply, self.0.listxattr_sized(ino, size), false);
    }

    /// Remove one xattr by UTF-8 name.
    ///
    /// Validates UTF-8. Delegates to ArkSession::removexattr which checks W,
    /// removes from attrs (commit_attrs fans out over hard links). Replies ok
    /// or errno.
    ///
    /// Preconditions: ino visible; caller has W; name exists or not (idempotent in practice).
    /// Postconditions: name gone from xattrs on all hard links of the inode.
    ///
    /// Maintainer: removal fans out via commit_attrs. See "xattr".
    fn removexattr(&mut self, req: &Request<'_>, ino: u64, name: &OsStr, reply: ReplyEmpty) {
        let Ok(name) = fuse_name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.removexattr(ino, name, req.uid(), req.gid()) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Directory fsync is a no-op: children persist on their own fsync/flush paths.
    ///
    /// Whole-object + per-inode buffer model means a directory has no data
    /// blob to fsync. Child files persist themselves. Always replies ok.
    ///
    /// Preconditions: fh from opendir.
    /// Postconditions: no change.
    ///
    /// Maintainer: children persist on their own fsync. Directory entries are
    /// in the temporal index, not a directory object. See "fsyncdir".
    fn fsyncdir(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    /// Return filesystem statistics: live path count, object bytes, free space.
    ///
    /// Delegates to ArkSession::statfs. Reports 4096-byte blocks, live
    /// non-tombstone path count as `files`, and backing-fs free space.
    /// On error replies errno.
    ///
    /// Preconditions: any ino (ignored).
    /// Postconditions: numbers reflect current live index + backing fs.
    ///
    /// Maintainer: files = live_path_count. Blocks may be synthetic when
    /// the backing fs cannot be statvfs'd. See "statfs" and usage().
    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        match self.0.statfs() {
            Ok(s) => reply.statfs(
                s.blocks, s.bfree, s.bavail, s.files, s.ffree, s.bsize, s.namelen, s.frsize,
            ),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Unix permission check (access(2)). default_permissions is off, kernel calls us.
    ///
    /// Delegates to ArkSession::access which uses require_mode against the
    /// live inode (R/W/X/F). F_OK is always true for an existing inode.
    /// Replies ok or EACCES via errno.
    ///
    /// Preconditions: ino visible in the view.
    /// Postconditions: no side effects (no atime update here).
    ///
    /// Maintainer: delegates to require_mode which uses arkfs_core::allows.
    /// See "Unix permission bits" and "access".
    fn access(&mut self, req: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
        match self.0.access(ino, mask, req.uid(), req.gid()) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// No device ioctls supported. Always returns ENOTTY.
    ///
    /// Regular files do not have associated device drivers in this
    /// implementation. _cmd / _in_data / _out_size are ignored.
    ///
    /// Preconditions: any ino/fh.
    /// Postconditions: reply.error(ENOTTY) is sent.
    ///
    /// Maintainer: ENOTTY is the POSIX answer for a regular file with no ioctl.
    /// See "ioctl".
    fn ioctl(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _flags: u32,
        _cmd: u32,
        _in_data: &[u8],
        _out_size: u32,
        reply: ReplyIoctl,
    ) {
        reply.error(ENOTTY);
    }

    /// Grow a regular file (mode 0 or FALLOC_FL_KEEP_SIZE).
    ///
    /// Only mode 0 (extend) and KEEP_SIZE are accepted. KEEP_SIZE is a no-op
    /// because we are whole-object. Otherwise delegates to setattr(size) which
    /// may dirty the inode buffer. Replies ok or errno.
    ///
    /// Preconditions: ino is regular; fh optional for size; offsets non-negative.
    /// Postconditions: file logical size is at least end of range (unless KEEP_SIZE).
    ///
    /// Maintainer: with KEEP_SIZE we are a no-op. Without it we extend via
    /// setattr. See "fallocate".
    fn fallocate(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        length: i64,
        mode: i32,
        reply: ReplyEmpty,
    ) {
        match self.0.fallocate(ino, Some(fh), offset, length, mode) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Reposition file offset or query hole/data (lseek).
    ///
    /// Delegates to ArkSession::lseek. SEEK_SET/END are normal. SEEK_DATA
    /// returns the given offset (whole-object). SEEK_HOLE returns EOF.
    /// Replies the resulting offset or errno.
    ///
    /// Preconditions: fh valid for the inode; offset sensible for the whence.
    /// Postconditions: no persistent change; position is returned to kernel.
    ///
    /// Maintainer: whole-object store means hole is always EOF. See "lseek".
    fn lseek(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        offset: i64,
        whence: i32,
        reply: ReplyLseek,
    ) {
        match self.0.lseek(ino, Some(fh), offset, whence) {
            Ok(pos) => reply.offset(pos),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// F_GETLK: return the conflicting lock, or F_UNLCK if the range is free.
    ///
    /// Delegates to ArkSession::getlk (which calls LockTable). Never blocks.
    /// On success replies the lock description; on error errno.
    ///
    /// Preconditions: ino is lockable (regular file).
    /// Postconditions: no state change.
    ///
    /// Maintainer: implemented in userspace via posix_lock::LockTable.
    /// Advertised via FUSE_POSIX_LOCKS. See "POSIX locks" and posix_lock.rs.
    fn getlk(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        lock_owner: u64,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
        reply: ReplyLock,
    ) {
        match self.0.getlk(ino, lock_owner, start, end, typ, pid) {
            Ok((start, end, typ, pid)) => reply.locked(start, end, typ, pid),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// F_SETLK / F_SETLKW. Set or wait for a POSIX record lock.
    ///
    /// For sleep=true (SETLKW) spawns a helper thread that calls setlk with wait,
    /// so the main session loop can still process the matching unlock. For
    /// non-blocking, calls directly. Replies via the original ReplyEmpty from
    /// the captured reply object.
    ///
    /// Preconditions: ino is lockable; for wait, lookup_ino succeeds.
    /// Postconditions: lock is acquired or error returned (EAGAIN, EDEADLK, etc.).
    ///
    /// Maintainer: SETLKW replies from a helper thread so the single-threaded
    /// session loop can still process the unlock. See "POSIX locks" and
    /// "SETLKW replies from a helper thread".
    fn setlk(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        lock_owner: u64,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
        sleep: bool,
        reply: ReplyEmpty,
    ) {
        if sleep {
            if let Err(e) = self.0.lookup_ino(ino) {
                reply.error(errno(e));
                return;
            }
            let table = self.0.lock_table();
            thread::spawn(
                move || match table.setlk(ino, lock_owner, start, end, typ, pid, true) {
                    Ok(()) => reply.ok(),
                    Err(e) => reply.error(errno(e)),
                },
            );
            return;
        }
        match self.0.setlk(ino, lock_owner, start, end, typ, pid, false) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Identity block map (not a block device).
    ///
    /// Returns the requested block index unchanged for regular files.
    /// Used by some tools that assume a block device; we are not one.
    ///
    /// Preconditions: ino exists.
    /// Postconditions: reply.bmap(idx) or error if blocksize==0.
    ///
    /// Maintainer: identity mapping only. See "bmap".
    fn bmap(&mut self, _req: &Request<'_>, ino: u64, blocksize: u32, idx: u64, reply: ReplyBmap) {
        match self.0.bmap(ino, blocksize, idx) {
            Ok(block) => reply.bmap(block),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Poll for events. Local files are always ready.
    ///
    /// Validates optional fh. Calls ArkSession::poll which always reports
    /// POLLIN|POLLOUT|POLLRDNORM|POLLWRNORM for regular files (no real async).
    /// Ignores ph/flags. Replies the intersection with requested events.
    ///
    /// Preconditions: ino exists; fh if non-zero must be valid.
    /// Postconditions: reply completed with ready mask or error.
    ///
    /// Maintainer: always-ready POLLIN|POLLOUT for local files. Implemented
    /// in userspace. See "poll" and "always-ready poll".
    fn poll(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        _ph: PollHandle,
        events: u32,
        _flags: u32,
        reply: ReplyPoll,
    ) {
        let fh = if fh == 0 { None } else { Some(fh) };
        match self.0.poll(ino, fh, events) {
            Ok(revents) => reply.poll(revents),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Copy bytes between two open handles (copy_file_range).
    ///
    /// Delegates to ArkSession::copy_file_range. Reads source into a temp
    /// buffer then writes to destination. Same-file overlapping copies are
    /// safe because source is snapshotted first. flags must be 0.
    ///
    /// Preconditions: both fhs are open regular files; offsets non-negative; len > 0.
    /// Postconditions: destination extended if needed; bytes copied.
    ///
    /// Maintainer: implemented via read into a temp buffer then write.
    /// Both source and dest must be open regular files. No cross-device.
    /// See "copy_file_range".
    fn copy_file_range(
        &mut self,
        _req: &Request<'_>,
        _ino_in: u64,
        fh_in: u64,
        offset_in: i64,
        _ino_out: u64,
        fh_out: u64,
        offset_out: i64,
        len: u64,
        flags: u32,
        reply: ReplyWrite,
    ) {
        match self
            .0
            .copy_file_range(fh_in, offset_in, fh_out, offset_out, len, flags)
        {
            Ok(n) => reply.written(n),
            Err(e) => reply.error(errno(e)),
        }
    }
}

/// Shared mount flags. No `AutoUnmount` (that implies `allow_other` on stock fuse.conf).
///
/// Always sets FSName. Adds RO when read_only. Never adds AutoUnmount.
///
/// Preconditions: name is a short identifier.
/// Postconditions: returned options are suitable for fuser::mount2 / spawn_mount2.
///
/// Maintainer: Do not pass AutoUnmount: it forces allow_other which stock
/// fuse.conf often disables. See "FUSE specifics" and "Production mount()".
fn mount_opts(name: &str, read_only: bool) -> Vec<MountOption> {
    let mut opts = vec![MountOption::FSName(name.into())];
    if read_only {
        opts.push(MountOption::RO);
    }
    opts
}

/// Foreground mount for `arkfs mount`. Blocks the calling thread until unmount.
///
/// Uses FuseFs wrapper around the provided ArkSession. Options come from
/// mount_opts (FSName + optional RO). Returns when the filesystem is unmounted
/// or on error.
///
/// Preconditions: mountpoint exists and is empty or a valid mount target;
/// session is the only user of the data dir for a writable mount.
/// Postconditions: on success the mount is active until unmounted externally.
///
/// Maintainer: foreground mount used by the CLI. See "mount" and
/// "Production `mount()` and tests use `FSName` plus `RO`".
pub fn mount(session: ArkSession, mountpoint: &Path) -> std::io::Result<()> {
    let opts = mount_opts("arkfs", session.is_read_only());
    fuser::mount2(FuseFs(session), mountpoint, &opts)
}

/// Background mount for the live FUSE test scaffold.
///
/// Spawns a background fuser session. The returned BackgroundSession handle
/// must be kept alive for the duration of the test mount. Uses "arkfs-test"
/// as FSName so tests can distinguish it.
///
/// Preconditions: mountpoint is available; no other process is using it.
/// Postconditions: mount is running in a helper thread; caller owns the handle.
///
/// Maintainer: used by libs/rust/fuse_facade/tests/fuse_drive.rs. Must skip
/// when CI or /dev/fuse missing. See "Live tests" and spawn.
pub fn spawn(session: ArkSession, mountpoint: &Path) -> std::io::Result<fuser::BackgroundSession> {
    let opts = mount_opts("arkfs-test", session.is_read_only());
    fuser::spawn_mount2(FuseFs(session), mountpoint, &opts)
}
