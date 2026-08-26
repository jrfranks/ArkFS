//! `fuser::Filesystem` adapter. Keep this file a translator.
//!
//! Each method: parse UTF-8 name → call [`ArkSession`] → `reply.*` or errno.
//! Do not implement mkdir/unlink semantics here.
//!
//! `mount` is the CLI path (`AutoUnmount` + `DefaultPermissions`). `spawn` is
//! the test path (no those flags — they interact badly with stock fuse.conf
//! `user_allow_other`). Prefer `spawn` flags for unprivileged mounts.
//!
//! `getlk`/`setlk` currently succeed as no-lock; `poll` returns 0 events.
//! fuser's ENOSYS default is usually better (in-kernel locks / always-ready
//! poll). See `docs/maintainer.md`.

use crate::session::{errno, fuse_kind, handle_to_attr, time_or_now_to_timespec, ArkSession, TTL};
use crate::xattr::SizedBytes;
use arkfs_core::PosixPatch;
use fuser::{
    FileType as FuseType, Filesystem, KernelConfig, MountOption, PollHandle, ReplyAttr, ReplyBmap,
    ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyIoctl, ReplyLock,
    ReplyLseek, ReplyOpen, ReplyPoll, ReplyStatfs, ReplyWrite, ReplyXattr, Request, TimeOrNow,
};
use libc::{c_int, ENOSYS, ENOTTY, EOPNOTSUPP, ERANGE};
use std::ffi::OsStr;
use std::path::Path;
use std::time::SystemTime;

/// Newtype over [`ArkSession`] so `Filesystem` impl stays in this module.
pub struct FuseFs(pub ArkSession);

/// UTF-8 FUSE names only. Non-UTF8 is EINVAL (same as the kernel-facing adapter).
pub fn fuse_name(name: &OsStr) -> Result<&str, i32> {
    name.to_str().ok_or(libc::EINVAL)
}

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

impl FuseFs {
    fn name(name: &OsStr) -> Result<&str, i32> {
        fuse_name(name)
    }
}

impl Filesystem for FuseFs {
    fn init(&mut self, _req: &Request<'_>, _config: &mut KernelConfig) -> Result<(), c_int> {
        Ok(())
    }

    fn destroy(&mut self) {}

    fn forget(&mut self, _req: &Request<'_>, _ino: u64, _nlookup: u64) {}

    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let Ok(name) = Self::name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.lookup(parent, name) {
            Ok(h) => reply.entry(&TTL, &handle_to_attr(&h), h.attrs.generation),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, fh: Option<u64>, reply: ReplyAttr) {
        match self.0.getattr(ino, fh) {
            Ok(a) => reply.attr(&TTL, &a),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn setattr(
        &mut self,
        _req: &Request<'_>,
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
        match self.0.setattr(ino, patch, fh) {
            Ok(a) => reply.attr(&TTL, &a),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn mknod(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        let Ok(name) = Self::name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        if mode & libc::S_IFMT != libc::S_IFREG {
            reply.error(EOPNOTSUPP);
            return;
        }
        match self
            .0
            .mknod_regular(parent, name, mode, req.uid(), req.gid())
        {
            Ok(h) => reply.entry(&TTL, &handle_to_attr(&h), h.attrs.generation),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn mkdir(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let Ok(name) = Self::name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.mkdir(parent, name, mode, req.uid(), req.gid()) {
            Ok(h) => reply.entry(&TTL, &handle_to_attr(&h), h.attrs.generation),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let Ok(name) = Self::name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.unlink(parent, name) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let Ok(name) = Self::name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.rmdir(parent, name) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn symlink(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        link_name: &OsStr,
        target: &std::path::Path,
        reply: ReplyEntry,
    ) {
        let Ok(name) = Self::name(link_name) else {
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

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.0.readlink(ino) {
            Ok(b) => reply.data(&b),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn link(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _newparent: u64,
        _newname: &OsStr,
        reply: ReplyEntry,
    ) {
        reply.error(EOPNOTSUPP);
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        let (Ok(name), Ok(newname)) = (Self::name(name), Self::name(newname)) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.rename(parent, name, newparent, newname) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        match self.0.open(ino, flags) {
            Ok(fh) => reply.opened(fh, 0),
            Err(e) => reply.error(errno(e)),
        }
    }

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

    fn flush(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        match self.0.fsync(fh) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

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

    fn release(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        match self.0.release(fh) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn opendir(&mut self, _req: &Request<'_>, _ino: u64, _flags: i32, reply: ReplyOpen) {
        reply.opened(0, 0);
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let h = match self.0.core.lookup_ino(ino, self.0.view) {
            Ok(h) => h,
            Err(e) => {
                reply.error(errno(e));
                return;
            }
        };
        let parent_ino = h
            .path
            .parent()
            .and_then(|p| self.0.core.lookup(p.as_str(), self.0.view).ok())
            .map(|p| p.attrs.file_id)
            .unwrap_or(ino);
        let mut entries: Vec<(u64, FuseType, String)> = vec![
            (ino, FuseType::Directory, ".".into()),
            (parent_ino, FuseType::Directory, "..".into()),
        ];
        match self.0.readdir(ino) {
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

    fn create(
        &mut self,
        req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let Ok(name) = Self::name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self
            .0
            .create_file(parent, name, mode, req.uid(), req.gid(), flags)
        {
            Ok((h, fh)) => reply.created(&TTL, &handle_to_attr(&h), h.attrs.generation, fh, 0),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn setxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        value: &[u8],
        _flags: i32,
        _position: u32,
        reply: ReplyEmpty,
    ) {
        let Ok(name) = Self::name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.setxattr(ino, name, value) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn getxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        let Ok(name) = Self::name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        reply_sized(reply, self.0.getxattr_sized(ino, name, size), true);
    }

    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        reply_sized(reply, self.0.listxattr_sized(ino, size), false);
    }

    fn removexattr(&mut self, _req: &Request<'_>, ino: u64, name: &OsStr, reply: ReplyEmpty) {
        let Ok(name) = Self::name(name) else {
            reply.error(libc::EINVAL);
            return;
        };
        match self.0.removexattr(ino, name) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

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

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        let n = self.0.object_count().unwrap_or(0);
        reply.statfs(n, n, n, n, n, 512, 255, 0);
    }

    fn access(&mut self, _req: &Request<'_>, ino: u64, _mask: i32, reply: ReplyEmpty) {
        match self.0.getattr(ino, None) {
            Ok(_) => reply.ok(),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn getlk(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        _start: u64,
        _end: u64,
        _typ: i32,
        _pid: u32,
        reply: ReplyLock,
    ) {
        reply.locked(0, 0, libc::F_UNLCK, 0);
    }

    fn setlk(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _lock_owner: u64,
        _start: u64,
        _end: u64,
        _typ: i32,
        _pid: u32,
        _sleep: bool,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn bmap(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _blocksize: u32,
        _idx: u64,
        reply: ReplyBmap,
    ) {
        reply.error(ENOSYS);
    }

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

    fn poll(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _ph: PollHandle,
        _events: u32,
        _flags: u32,
        reply: ReplyPoll,
    ) {
        reply.poll(0);
    }

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
}

/// Foreground mount for `arkfs mount`. Blocks until unmount.
pub fn mount(session: ArkSession, mountpoint: &Path) -> std::io::Result<()> {
    let mut opts = vec![
        MountOption::FSName("arkfs".into()),
        MountOption::DefaultPermissions,
        MountOption::AutoUnmount,
    ];
    if session.read_only {
        opts.push(MountOption::RO);
    }
    fuser::mount2(FuseFs(session), mountpoint, &opts)
}

/// Background mount for the live FUSE test scaffold. No DefaultPermissions/AutoUnmount.
pub fn spawn(session: ArkSession, mountpoint: &Path) -> std::io::Result<fuser::BackgroundSession> {
    let mut opts = vec![MountOption::FSName("arkfs-test".into())];
    if session.read_only {
        opts.push(MountOption::RO);
    }
    fuser::spawn_mount2(FuseFs(session), mountpoint, &opts)
}
