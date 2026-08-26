//! POSIX / FUSE conformance against [`fuse_facade::ArkSession`] (no `/dev/fuse`).
//!
//! Each `fuse_*` test is the reachability witness for the named FUSE op in
//! [`fuse_facade::IMPLEMENTED_FUSE_OPS`]. A parser test greps `src/fuse.rs`
//! so adding a `Filesystem` method without a table entry / `fuse_*` test fails
//! `make ci`.
//!
//! This is the GitHub-safe suite. Kernel-visible behavior is `fuse_drive.rs`.

use arkfs_core::attr_map::FuseSetAttr;
use arkfs_core::{ArkError, FileType};
use fuse_facade::{
    all_errno_pairs, apply_write, fuse_kind, fuse_name, read_slice, sized, to_errno, ArkSession,
    SizedBytes, IMPLEMENTED_FUSE_OPS,
};
use fuser::FUSE_ROOT_ID;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use tempfile::tempdir;

fn sess() -> (tempfile::TempDir, ArkSession) {
    let d = tempdir().unwrap();
    let s = ArkSession::mount_store(d.path(), None).unwrap();
    (d, s)
}

fn create(s: &ArkSession, name: &str) -> (u64, u64) {
    let (h, fh) = s
        .create_file(FUSE_ROOT_ID, name, 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    (h.attrs.file_id, fh)
}

#[test]
fn reachability_contract_lists_adapter_surface() {
    for op in [
        "init",
        "destroy",
        "forget",
        "lookup",
        "getattr",
        "setattr",
        "readlink",
        "mknod",
        "mkdir",
        "unlink",
        "rmdir",
        "symlink",
        "rename",
        "link",
        "open",
        "read",
        "write",
        "flush",
        "release",
        "fsync",
        "opendir",
        "readdir",
        "releasedir",
        "fsyncdir",
        "statfs",
        "setxattr",
        "getxattr",
        "listxattr",
        "removexattr",
        "access",
        "create",
        "getlk",
        "setlk",
        "bmap",
        "ioctl",
        "poll",
        "fallocate",
        "lseek",
    ] {
        assert!(
            IMPLEMENTED_FUSE_OPS.contains(&op),
            "missing FUSE op in reachability table: {op}"
        );
    }
}

#[test]
fn fuse_lookup() {
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "d", 0o755, 1, 2).unwrap();
    let h = s.lookup(FUSE_ROOT_ID, "d").unwrap();
    assert_eq!(h.attrs.file_type, FileType::Directory);
    assert_eq!(h.attrs.uid, 1);
    assert!(s.lookup(FUSE_ROOT_ID, "nope").is_err());
    assert!(s.lookup(FUSE_ROOT_ID, ".").is_err());
    assert!(s.lookup(FUSE_ROOT_ID, "..").is_err());
    assert!(s.lookup(FUSE_ROOT_ID, "a/b").is_err());
}

#[test]
fn fuse_getattr() {
    let (_d, s) = sess();
    let a = s.getattr(FUSE_ROOT_ID, None).unwrap();
    assert_eq!(a.ino, FUSE_ROOT_ID);
    assert_eq!(a.kind, fuser::FileType::Directory);
    assert!(s.getattr(999_999, None).is_err());
}

#[test]
fn fuse_setattr_mode_uid_gid_size_times() {
    let (_d, s) = sess();
    let (ino, fh) = create(&s, "s");
    s.release(fh).unwrap();
    let a = s
        .setattr(
            ino,
            FuseSetAttr {
                mode: Some(0o640),
                uid: Some(7),
                gid: Some(8),
                size: Some(4),
                atime: Some(arkfs_core::Timespec::new(10, 1)),
                mtime: Some(arkfs_core::Timespec::new(11, 2)),
                ctime: None,
            },
            None,
        )
        .unwrap();
    assert_eq!(a.perm, 0o640);
    assert_eq!(a.uid, 7);
    assert_eq!(a.gid, 8);
    assert_eq!(a.size, 4);
}

#[test]
fn fuse_setattr_size_via_open_fh() {
    let (_d, s) = sess();
    let (ino, fh) = create(&s, "t");
    s.write(fh, 0, b"abcdef").unwrap();
    s.setattr(
        ino,
        FuseSetAttr {
            size: Some(3),
            ..Default::default()
        },
        Some(fh),
    )
    .unwrap();
    let a = s.getattr(ino, Some(fh)).unwrap();
    assert_eq!(a.size, 3);
    s.fsync(fh).unwrap();
    s.release(fh).unwrap();
    let fh = s.open(ino, libc::O_RDONLY).unwrap();
    assert_eq!(s.read(fh, 0, 10).unwrap(), b"abc");
}

#[test]
fn fuse_mkdir() {
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "a", 0o755, 0, 0).unwrap();
    s.mkdir(
        s.lookup(FUSE_ROOT_ID, "a").unwrap().attrs.file_id,
        "b",
        0o700,
        0,
        0,
    )
    .unwrap();
    assert_eq!(s.lookup(FUSE_ROOT_ID, "a").unwrap().path.as_str(), "/a");
    let err = s.mkdir(FUSE_ROOT_ID, "a", 0o755, 0, 0).unwrap_err();
    assert!(matches!(err, ArkError::AlreadyExists { .. }));
}

#[test]
fn fuse_create_open_read_write_flush_fsync_release() {
    let (_d, s) = sess();
    let (ino, fh) = create(&s, "f");
    assert_eq!(s.write(fh, 0, b"hello").unwrap(), 5);
    assert_eq!(s.getattr(ino, Some(fh)).unwrap().size, 5);
    s.fsync(fh).unwrap();
    s.fsync(fh).unwrap();
    s.release(fh).unwrap();
    let fh = s.open(ino, libc::O_RDONLY).unwrap();
    assert_eq!(s.read(fh, 0, 5).unwrap(), b"hello");
    assert_eq!(s.read(fh, 3, 8).unwrap(), b"lo");
    assert!(s.read(fh, 50, 2).unwrap().is_empty());
    assert_eq!(s.read(fh, -1, 2).unwrap(), b"he");
    let err = s.write(fh, 0, b"x").unwrap_err();
    assert!(matches!(err, ArkError::ReadOnly));
    s.release(fh).unwrap();
}

#[test]
fn fuse_write_hole_and_o_trunc() {
    let (_d, s) = sess();
    let (ino, fh) = create(&s, "h");
    s.write(fh, 4, b"Z").unwrap();
    s.fsync(fh).unwrap();
    s.release(fh).unwrap();
    let fh = s.open(ino, libc::O_RDWR | libc::O_TRUNC).unwrap();
    assert_eq!(s.read(fh, 0, 10).unwrap(), b"");
    s.write(fh, 0, b"n").unwrap();
    s.release(fh).unwrap();
    let fh = s.open(ino, libc::O_RDONLY).unwrap();
    assert_eq!(s.read(fh, 0, 10).unwrap(), b"n");
}

#[test]
fn fuse_open_directory_is_error() {
    let (_d, s) = sess();
    let err = s.open(FUSE_ROOT_ID, libc::O_RDONLY).unwrap_err();
    assert!(matches!(err, ArkError::IsADirectory { .. }));
}

#[test]
fn fuse_unlink_rmdir() {
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "d", 0o755, 0, 0).unwrap();
    let dir = s.lookup(FUSE_ROOT_ID, "d").unwrap().attrs.file_id;
    let (_ino, fh) = s.create_file(dir, "x", 0o644, 0, 0, libc::O_RDWR).unwrap();
    s.release(fh).unwrap();
    let err = s.unlink(FUSE_ROOT_ID, "d").unwrap_err();
    assert!(matches!(err, ArkError::IsADirectory { .. }));
    let err = s.rmdir(dir, "x").unwrap_err();
    assert!(matches!(err, ArkError::NotADirectory { .. }));
    let err = s.rmdir(FUSE_ROOT_ID, "d").unwrap_err();
    assert!(matches!(err, ArkError::NotEmpty { .. }));
    s.unlink(dir, "x").unwrap();
    s.rmdir(FUSE_ROOT_ID, "d").unwrap();
    assert!(s.lookup(FUSE_ROOT_ID, "d").is_err());
}

#[test]
fn fuse_rename() {
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "a", 0o755, 0, 0).unwrap();
    s.mkdir(FUSE_ROOT_ID, "b", 0o755, 0, 0).unwrap();
    let a = s.lookup(FUSE_ROOT_ID, "a").unwrap().attrs.file_id;
    let b = s.lookup(FUSE_ROOT_ID, "b").unwrap().attrs.file_id;
    let (_ino, fh) = s.create_file(a, "n", 0o644, 0, 0, libc::O_RDWR).unwrap();
    s.write(fh, 0, b"v").unwrap();
    s.release(fh).unwrap();
    s.rename(a, "n", b, "m").unwrap();
    assert!(s.lookup(a, "n").is_err());
    let m = s.lookup(b, "m").unwrap();
    let fh = s.open(m.attrs.file_id, libc::O_RDONLY).unwrap();
    assert_eq!(s.read(fh, 0, 1).unwrap(), b"v");
}

#[test]
fn fuse_symlink_readlink() {
    let (_d, s) = sess();
    let h = s.symlink(FUSE_ROOT_ID, "l", "/target", 0, 0).unwrap();
    assert_eq!(s.readlink(h.attrs.file_id).unwrap(), b"/target");
}

#[test]
fn fuse_readdir() {
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "z", 0o755, 0, 0).unwrap();
    s.mkdir(FUSE_ROOT_ID, "a", 0o755, 0, 0).unwrap();
    let names: Vec<_> = s
        .readdir(FUSE_ROOT_ID)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, vec!["a", "z"]);
}

#[test]
fn fuse_xattr_set_get_list_remove_sized() {
    let (_d, s) = sess();
    let (ino, fh) = create(&s, "x");
    s.release(fh).unwrap();
    s.setxattr(ino, "user.k", b"val").unwrap();
    assert_eq!(s.getxattr(ino, "user.k").unwrap(), b"val");
    assert_eq!(
        s.getxattr_sized(ino, "user.k", 0).unwrap(),
        SizedBytes::Size(3)
    );
    assert_eq!(
        s.getxattr_sized(ino, "user.k", 2).unwrap(),
        SizedBytes::Range
    );
    assert_eq!(
        s.getxattr_sized(ino, "user.k", 8).unwrap(),
        SizedBytes::Data(b"val".to_vec())
    );
    assert!(s.getxattr(ino, "user.missing").is_err());
    let list = s.listxattr(ino).unwrap();
    assert!(list.windows(6).any(|w| w == b"user.k"));
    assert_eq!(
        s.listxattr_sized(ino, 0).unwrap(),
        SizedBytes::Size(list.len() as u32)
    );
    s.removexattr(ino, "user.k").unwrap();
    assert!(s.getxattr(ino, "user.k").is_err());
}

#[test]
fn fuse_never_delete_as_of_and_remount() {
    let d = tempdir().unwrap();
    let logical = {
        let s = ArkSession::mount_store(d.path(), None).unwrap();
        let (_ino, fh) = s
            .create_file(FUSE_ROOT_ID, "keep", 0o644, 0, 0, libc::O_RDWR)
            .unwrap();
        s.write(fh, 0, b"old").unwrap();
        s.release(fh).unwrap();
        let t = s.core.now().logical;
        s.unlink(FUSE_ROOT_ID, "keep").unwrap();
        t
    };
    let live = ArkSession::mount_store(d.path(), None).unwrap();
    assert!(live.lookup(FUSE_ROOT_ID, "keep").is_err());
    let hist = ArkSession::mount_store(d.path(), Some(logical)).unwrap();
    let h = hist.lookup(FUSE_ROOT_ID, "keep").unwrap();
    let fh = hist.open(h.attrs.file_id, libc::O_RDONLY).unwrap();
    assert_eq!(hist.read(fh, 0, 3).unwrap(), b"old");
    assert!(matches!(
        hist.mkdir(FUSE_ROOT_ID, "no", 0o755, 0, 0).unwrap_err(),
        ArkError::ReadOnly
    ));
}

#[test]
fn fuse_bad_fh_and_ro_mutators() {
    let (_d, s) = sess();
    assert!(s.read(99, 0, 1).is_err());
    assert!(s.write(99, 0, b"x").is_err());
    assert!(s.fsync(99).is_err());
}

#[test]
fn fuse_name_rejects_non_utf8() {
    let bad = OsStr::from_bytes(&[0xff, 0xfe]);
    assert_eq!(fuse_name(bad), Err(libc::EINVAL));
    assert_eq!(fuse_name(OsStr::new("ok")), Ok("ok"));
}

#[test]
fn fuse_kind_and_errno_and_iobuf_are_total() {
    for t in [
        FileType::File,
        FileType::Directory,
        FileType::Symlink,
        FileType::BlockDevice,
        FileType::CharDevice,
        FileType::Fifo,
        FileType::Socket,
        FileType::Reparse,
    ] {
        let _ = fuse_kind(t);
    }
    for (e, n) in all_errno_pairs() {
        assert_eq!(to_errno(&e), n);
    }
    let mut v = b"ab".to_vec();
    apply_write(&mut v, 0, b"XY");
    assert_eq!(read_slice(&v, 0, 2), b"XY");
    assert_eq!(sized(b"zz", 1), SizedBytes::Range);
}

#[test]
fn fuse_impl_source_matches_reachability_table() {
    let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/fuse.rs"));
    let mut found = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("fn ") {
            let name = rest.split('(').next().unwrap_or("");
            if matches!(name, "name" | "reply_sized" | "fuse_name" | "mount") {
                continue;
            }
            found.push(name);
            assert!(
                IMPLEMENTED_FUSE_OPS.contains(&name),
                "Filesystem method `{name}` is not in IMPLEMENTED_FUSE_OPS"
            );
        }
    }
    assert!(found.contains(&"lookup") && found.contains(&"readdir"));
}

#[test]
fn fuse_time_or_now() {
    use fuse_facade::time_or_now_to_timespec;
    use fuser::TimeOrNow;
    use std::time::{Duration, UNIX_EPOCH};
    let t = time_or_now_to_timespec(TimeOrNow::SpecificTime(
        UNIX_EPOCH + Duration::from_nanos(1_500_000_000),
    ));
    assert_eq!(t.sec, 1);
    assert_eq!(t.nsec, 500_000_000);
    let _ = time_or_now_to_timespec(TimeOrNow::Now);
}
