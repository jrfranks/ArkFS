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
pub struct FuseFs(pub ArkSession);

/// UTF-8 FUSE names only. Non-UTF8 is EINVAL (same as the kernel-facing adapter).
pub fn fuse_name(name: &OsStr) -> Result<&str, i32> {
    name.to_str().ok_or(libc::EINVAL)
}

/// FUSE xattr reply: size / data / ERANGE. Missing getxattr is ENODATA.
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
    fn destroy(&mut self) {
        let _ = self.0.flush_dirty();
    }

    /// Kernel dropped a lookup ref. Inodes are durable file_ids; we do not evict.
    fn forget(&mut self, _req: &Request<'_>, _ino: u64, _nlookup: u64) {}

    /// UTF-8 name in parent → entry. Non-UTF8 is EINVAL.
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

    /// Stat. Passes fh so dirty size is visible.
    fn getattr(&mut self, _req: &Request<'_>, ino: u64, fh: Option<u64>, reply: ReplyAttr) {
        match self.0.getattr(ino, fh) {
            Ok(a) => reply.attr(&TTL, &a),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Partial POSIX setattr (mode/uid/gid/size/times). Unset fields stay put.
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

    /// Create regular/fifo/socket/device. mode includes S_IFMT; rdev for devices.
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

    /// Tombstone a non-directory name.
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

    /// Tombstone an empty directory.
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

    /// Create a symlink; target must be UTF-8.
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
    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.0.readlink(ino) {
            Ok(b) => reply.data(&b),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Hard link: extra name for ino in newparent.
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

    /// Move a name. flags are Linux renameat2 bits.
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

    /// Open a non-directory; returns fh for the shared inode buffer.
    fn open(&mut self, req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        match self.0.open(ino, flags, req.uid(), req.gid()) {
            Ok(fh) => reply.opened(fh, 0),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Read from the open inode buffer.
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

    /// Write into the open inode buffer (regular files).
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

    /// FUSE close: persist bytes and drop POSIX locks for lock_owner.
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

    /// Persist dirty bytes for fh. Does not drop locks.
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

    /// Last close of fh: persist, drop locks, drop the handle.
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

    /// Directory open: must be a directory; search permission required.
    fn opendir(&mut self, req: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        match self.0.opendir(ino, req.uid(), req.gid()) {
            Ok(fh) => reply.opened(fh, 0),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// List children plus `.` / `..`. Offset is the next entry index.
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

    /// readdir with attributes (LOOKUP packed in).
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

    /// Directory close. Nothing to persist.
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

    /// Create + open a regular file in one request.
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

    /// Set one xattr (UTF-8 name).
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

    /// Get one xattr. Missing is ENODATA.
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

    /// List xattr names (NUL-separated).
    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        reply_sized(reply, self.0.listxattr_sized(ino, size), false);
    }

    /// Remove one xattr.
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

    /// Directory fsync is a no-op: children persist on their own fsync.
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

    /// Live path count, object-store usage, backing-fs free space. 4096-byte blocks.
    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        match self.0.statfs() {
            Ok(s) => reply.statfs(
                s.blocks, s.bfree, s.bavail, s.files, s.ffree, s.bsize, s.namelen, s.frsize,
            ),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Unix permission check. default_permissions is off, so the kernel calls this.
    fn access(&mut self, req: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
        match self.0.access(ino, mask, req.uid(), req.gid()) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// No device ioctls. ENOTTY is the POSIX answer for a regular file.
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

    /// Grow a regular file (mode 0 or KEEP_SIZE).
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

    /// SEEK_SET/END/DATA/HOLE. Whole-object store: hole is EOF.
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

    /// F_GETLK: conflicting lock, or F_UNLCK if free.
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

    /// F_SETLK / F_SETLKW. Blocking wait replies from a helper thread.
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

    /// Identity block map (not a blkdev).
    fn bmap(&mut self, _req: &Request<'_>, ino: u64, blocksize: u32, idx: u64, reply: ReplyBmap) {
        match self.0.bmap(ino, blocksize, idx) {
            Ok(block) => reply.bmap(block),
            Err(e) => reply.error(errno(e)),
        }
    }

    /// Always-ready POLLIN|POLLOUT for local files.
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

    /// Copy bytes between two open handles.
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
fn mount_opts(name: &str, read_only: bool) -> Vec<MountOption> {
    let mut opts = vec![MountOption::FSName(name.into())];
    if read_only {
        opts.push(MountOption::RO);
    }
    opts
}

/// Foreground mount for `arkfs mount`. Blocks until unmount.
pub fn mount(session: ArkSession, mountpoint: &Path) -> std::io::Result<()> {
    let opts = mount_opts("arkfs", session.is_read_only());
    fuser::mount2(FuseFs(session), mountpoint, &opts)
}

/// Background mount for the live FUSE test scaffold.
pub fn spawn(session: ArkSession, mountpoint: &Path) -> std::io::Result<fuser::BackgroundSession> {
    let opts = mount_opts("arkfs-test", session.is_read_only());
    fuser::spawn_mount2(FuseFs(session), mountpoint, &opts)
}
