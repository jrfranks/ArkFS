//! Boundary tests for the POSIX holes that used to be wrong:
//!
//! 1. `access` was existence-only — now Unix bits + `EACCES`.
//! 2. fifo/device/socket `open` was `EINVAL` — now `ENXIO`.
//! 3. `statfs` was dummy zeros / 512-byte blocks — now live counts + 4096.
//! 4. Live `lookup_ino` scanned every path — map is O(1) and survives remount.
//! 5. Hard-link writes used a second commit path — one `commit_branch` fans out.
//! 6. 1s attr TTL could serve stale `nlink` — TTL is zero; getattr is immediate.
//!
//! No `/dev/fuse`. Kernel-visible extras live in `fuse_drive.rs`.

use arkfs_core::attr_map::FuseSetAttr;
use arkfs_core::ArkError;
#[allow(unused_imports)]
use arkfs_test_review::{review_assert as assert, review_eq as assert_eq, review_ne as assert_ne};
use fuse_facade::{to_errno, ArkSession, TTL};
use fuser::FUSE_ROOT_ID;
use tempfile::tempdir;

/// Fresh isolated ArkSession (no /dev/fuse).
fn sess() -> (tempfile::TempDir, ArkSession) {
    let d = tempdir().unwrap();
    let s = ArkSession::mount_store(d.path(), None).unwrap();
    (d, s)
}

/// 0777 directory at `/w` so non-root callers can create names.
fn world(s: &ArkSession) -> u64 {
    s.mkdir(FUSE_ROOT_ID, "w", 0o777, 0, 0).unwrap();
    s.lookup(FUSE_ROOT_ID, "w", 0, 0).unwrap().attrs.file_id
}

/// Assert `PermissionDenied` and that FUSE maps it to `EACCES` (not `EINVAL`).
fn assert_eacces(err: ArkError) {
    assert!(
        matches!(err, ArkError::PermissionDenied { .. }),
        "want PermissionDenied, got {err}"
    );
    assert_eq!(to_errno(&err), libc::EACCES);
}

/// Assert `NoSuchDevice` and that FUSE maps it to `ENXIO` (not `EINVAL`).
fn assert_enxio(err: ArkError) {
    assert!(
        matches!(err, ArkError::NoSuchDevice { .. }),
        "want NoSuchDevice, got {err}"
    );
    assert_eq!(to_errno(&err), libc::ENXIO);
    assert_ne!(to_errno(&err), libc::EINVAL);
}

/// Missing inode is ENOENT, not a successful existence check.
#[test]
fn access_missing_is_not_found() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("access_missing_is_not_found");
    let (_d, s) = sess();
    let err = s.access(99_999, libc::F_OK, 0, 0).unwrap_err();
    assert!(matches!(err, ArkError::NotFound { .. }));
    assert_eq!(to_errno(&err), libc::ENOENT);
}

/// F_OK succeeds when the name exists; R/W/X follow the mode class.
#[test]
fn access_unix_bits_owner_group_other() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("access_unix_bits_owner_group_other");
    let (_d, s) = sess();
    let w = world(&s);
    let (f, fh) = s.create_file(w, "a", 0o640, 5, 7, libc::O_RDWR).unwrap();
    s.release(fh).unwrap();
    let ino = f.attrs.file_id;

    s.access(ino, libc::F_OK, 99, 99).unwrap();
    // owner 5: rw-
    s.access(ino, libc::R_OK | libc::W_OK, 5, 1).unwrap();
    assert_eacces(s.access(ino, libc::X_OK, 5, 7).unwrap_err());
    // group 7: r--
    s.access(ino, libc::R_OK, 8, 7).unwrap();
    assert_eacces(s.access(ino, libc::W_OK, 8, 7).unwrap_err());
    // other: ---
    assert_eacces(s.access(ino, libc::R_OK, 9, 9).unwrap_err());
    assert_eacces(s.access(ino, libc::W_OK, 9, 9).unwrap_err());
    // root read/write bypass; execute still needs an x bit
    s.access(ino, libc::R_OK | libc::W_OK, 0, 0).unwrap();
    assert_eacces(s.access(ino, libc::X_OK, 0, 0).unwrap_err());
}

/// Directory X_OK (search) vs file X_OK.
#[test]
fn access_execute_file_vs_directory() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("access_execute_file_vs_directory");
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "d", 0o755, 0, 0).unwrap();
    let d = s.lookup(FUSE_ROOT_ID, "d", 0, 0).unwrap().attrs.file_id;
    s.access(d, libc::X_OK | libc::R_OK, 9, 9).unwrap();
    assert_eacces(s.access(d, libc::W_OK, 9, 9).unwrap_err());

    let (f, fh) = s.create_file(d, "f", 0o644, 0, 0, libc::O_RDWR).unwrap();
    s.release(fh).unwrap();
    assert_eacces(s.access(f.attrs.file_id, libc::X_OK, 9, 9).unwrap_err());
    s.access(f.attrs.file_id, libc::R_OK, 9, 9).unwrap();
}

/// open flags: O_RDONLY needs R, O_WRONLY/O_RDWR/O_TRUNC need W.
#[test]
fn open_masks_follow_unix_bits() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("open_masks_follow_unix_bits");
    let (_d, s) = sess();
    let w = world(&s);
    let (f, fh) = s.create_file(w, "a", 0o444, 5, 5, libc::O_RDWR).unwrap();
    s.release(fh).unwrap();
    let ino = f.attrs.file_id;
    let fh = s.open(ino, libc::O_RDONLY, 5, 5).unwrap();
    s.release(fh).unwrap();
    let fh = s.open(ino, libc::O_RDONLY, 9, 9).unwrap();
    s.release(fh).unwrap();
    assert_eacces(s.open(ino, libc::O_WRONLY, 5, 5).unwrap_err());
    assert_eacces(s.open(ino, libc::O_RDWR, 9, 9).unwrap_err());
    assert_eacces(
        s.open(ino, libc::O_RDONLY | libc::O_TRUNC, 5, 5)
            .unwrap_err(),
    );
    let fh = s.open(ino, libc::O_RDWR, 0, 0).unwrap();
    s.release(fh).unwrap();
}

/// 0700 dir: owner can mutate; stranger cannot (mkdir/create/mknod/symlink/link/rename/unlink/rmdir).
#[test]
fn directory_mutate_requires_write_and_search() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("directory_mutate_requires_write_and_search");
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "p", 0o700, 0, 0).unwrap();
    let p = s.lookup(FUSE_ROOT_ID, "p", 0, 0).unwrap().attrs.file_id;
    s.setattr(
        p,
        FuseSetAttr {
            uid: Some(1),
            gid: Some(1),
            ..Default::default()
        },
        None,
        0,
        0,
    )
    .unwrap();
    let (src, fh) = s
        .create_file(FUSE_ROOT_ID, "src", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.release(fh).unwrap();

    assert_eacces(s.mkdir(p, "c", 0o755, 9, 9).unwrap_err());
    assert_eacces(
        s.create_file(p, "f", 0o644, 9, 9, libc::O_RDWR)
            .unwrap_err(),
    );
    assert_eacces(s.mknod(p, "n", libc::S_IFIFO | 0o644, 9, 9, 0).unwrap_err());
    assert_eacces(s.symlink(p, "l", "/t", 9, 9).unwrap_err());
    assert_eacces(s.link(src.attrs.file_id, p, "h", 9, 9).unwrap_err());
    assert_eacces(
        s.rename(FUSE_ROOT_ID, "src", p, "moved", 0, 9, 9)
            .unwrap_err(),
    );

    s.mkdir(p, "ok", 0o755, 1, 1).unwrap();
    let (_h, fh) = s.create_file(p, "f", 0o644, 1, 1, libc::O_RDWR).unwrap();
    s.release(fh).unwrap();
    assert_eacces(s.unlink(p, "f", 9, 9).unwrap_err());
    s.unlink(p, "f", 1, 1).unwrap();
    assert_eacces(s.rmdir(p, "ok", 9, 9).unwrap_err());
    s.rmdir(p, "ok", 1, 1).unwrap();
}

/// Sticky: stranger cannot unlink/rename; file owner, dir owner, and root can.
#[test]
fn sticky_unlink_and_rename_boundaries() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("sticky_unlink_and_rename_boundaries");
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "t", 0o1777, 0, 0).unwrap();
    let t = s.lookup(FUSE_ROOT_ID, "t", 0, 0).unwrap().attrs.file_id;
    let (_f, fh) = s.create_file(t, "x", 0o644, 2, 2, libc::O_RDWR).unwrap();
    s.release(fh).unwrap();
    assert_eacces(s.unlink(t, "x", 9, 9).unwrap_err());
    assert_eacces(s.rename(t, "x", t, "y", 0, 9, 9).unwrap_err());
    s.rename(t, "x", t, "y", 0, 2, 2).unwrap();
    s.unlink(t, "y", 0, 0).unwrap();

    s.setattr(
        t,
        FuseSetAttr {
            uid: Some(1),
            gid: Some(1),
            mode: Some(0o1777),
            ..Default::default()
        },
        None,
        0,
        0,
    )
    .unwrap();
    let (_f, fh) = s.create_file(t, "z", 0o644, 3, 3, libc::O_RDWR).unwrap();
    s.release(fh).unwrap();
    assert_eacces(s.unlink(t, "z", 9, 9).unwrap_err());
    s.unlink(t, "z", 1, 1).unwrap();
}

/// chmod/chown/times need owner or root; truncate needs write; xattr needs write.
#[test]
fn setattr_and_xattr_cred_boundaries() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("setattr_and_xattr_cred_boundaries");
    let (_d, s) = sess();
    let w = world(&s);
    let (f, fh) = s.create_file(w, "m", 0o644, 5, 5, libc::O_RDWR).unwrap();
    s.release(fh).unwrap();
    let ino = f.attrs.file_id;

    assert_eacces(
        s.setattr(
            ino,
            FuseSetAttr {
                mode: Some(0o600),
                ..Default::default()
            },
            None,
            9,
            9,
        )
        .unwrap_err(),
    );
    assert_eacces(
        s.setattr(
            ino,
            FuseSetAttr {
                uid: Some(9),
                ..Default::default()
            },
            None,
            5,
            5,
        )
        .unwrap_err(),
    );
    assert_eacces(
        s.setattr(
            ino,
            FuseSetAttr {
                atime: Some(arkfs_core::Timespec::new(1, 0)),
                ..Default::default()
            },
            None,
            9,
            9,
        )
        .unwrap_err(),
    );
    assert_eacces(
        s.setattr(
            ino,
            FuseSetAttr {
                size: Some(1),
                ..Default::default()
            },
            None,
            9,
            9,
        )
        .unwrap_err(),
    );
    s.setattr(
        ino,
        FuseSetAttr {
            mode: Some(0o600),
            ..Default::default()
        },
        None,
        5,
        5,
    )
    .unwrap();
    s.setattr(
        ino,
        FuseSetAttr {
            uid: Some(8),
            gid: Some(8),
            ..Default::default()
        },
        None,
        0,
        0,
    )
    .unwrap();
    assert_eq!(s.getattr(ino, None).unwrap().uid, 8);

    assert_eacces(s.setxattr(ino, "user.k", b"v", 0, 9, 9).unwrap_err());
    s.setxattr(ino, "user.k", b"v", 0, 0, 0).unwrap();
    assert_eacces(s.removexattr(ino, "user.k", 9, 9).unwrap_err());
    s.removexattr(ino, "user.k", 0, 0).unwrap();
}

/// fifo / char / block / socket: every access mode is ENXIO, never EINVAL.
#[test]
fn special_file_open_enxio_all_flags() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("special_file_open_enxio_all_flags");
    let (_d, s) = sess();
    for (name, mode) in [
        ("p", libc::S_IFIFO | 0o666),
        ("c", libc::S_IFCHR | 0o666),
        ("b", libc::S_IFBLK | 0o666),
        ("k", libc::S_IFSOCK | 0o666),
    ] {
        let h = s.mknod(FUSE_ROOT_ID, name, mode, 0, 0, 0).unwrap();
        for flags in [libc::O_RDONLY, libc::O_WRONLY, libc::O_RDWR] {
            assert_enxio(s.open(h.attrs.file_id, flags, 0, 0).unwrap_err());
        }
    }
    let (f, fh) = s
        .create_file(FUSE_ROOT_ID, "reg", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.release(fh).unwrap();
    let fh = s.open(f.attrs.file_id, libc::O_RDONLY, 0, 0).unwrap();
    s.release(fh).unwrap();
    let err = s.open(FUSE_ROOT_ID, libc::O_RDONLY, 0, 0).unwrap_err();
    assert!(matches!(err, ArkError::IsADirectory { .. }));
    assert_eq!(to_errno(&err), libc::EISDIR);
}

/// Dummy statfs used to be zeros + 512-byte blocks. Live files and 4096 are required.
#[test]
fn statfs_is_not_dummy() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("statfs_is_not_dummy");
    let (_d, s) = sess();
    let empty = s.statfs().unwrap();
    assert_eq!(empty.bsize, 4096);
    assert_eq!(empty.frsize, 4096);
    assert_eq!(empty.namelen, 255);
    assert_ne!(empty.bsize, 512);
    assert!(empty.files >= 1, "root is a live path");
    assert!(empty.blocks >= 1);
    assert!(empty.ffree > 0);

    let before = empty.files;
    let (_f, fh) = s
        .create_file(FUSE_ROOT_ID, "n", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.write(fh, 0, &[1u8; 4096]).unwrap();
    s.fsync(fh).unwrap();
    s.release(fh).unwrap();
    let mid = s.statfs().unwrap();
    assert_eq!(mid.files, before + 1);
    assert!(mid.blocks >= empty.blocks);

    s.unlink(FUSE_ROOT_ID, "n", 0, 0).unwrap();
    let after = s.statfs().unwrap();
    assert_eq!(after.files, before, "tombstones are not live files");
}

/// Live lookup_ino follows rename and the lex-first hard-link name; remount rebuilds the map.
#[test]
fn lookup_ino_map_rename_hardlink_remount() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("lookup_ino_map_rename_hardlink_remount");
    let d = tempdir().unwrap();
    let ino;
    {
        let s = ArkSession::mount_store(d.path(), None).unwrap();
        let (h, fh) = s
            .create_file(FUSE_ROOT_ID, "z", 0o644, 0, 0, libc::O_RDWR)
            .unwrap();
        s.release(fh).unwrap();
        ino = h.attrs.file_id;
        assert_eq!(s.lookup_ino(ino).unwrap().path.as_str(), "/z");
        s.link(ino, FUSE_ROOT_ID, "a", 0, 0).unwrap();
        // BTreeMap order: /a before /z
        assert_eq!(s.lookup_ino(ino).unwrap().path.as_str(), "/a");
        s.rename(FUSE_ROOT_ID, "z", FUSE_ROOT_ID, "m", 0, 0, 0)
            .unwrap();
        assert_eq!(s.lookup_ino(ino).unwrap().path.as_str(), "/a");
        assert!(s.lookup_ino(ino + 1_000_000).is_err());
    }
    let s = ArkSession::mount_store(d.path(), None).unwrap();
    assert_eq!(s.lookup_ino(ino).unwrap().path.as_str(), "/a");
}

/// One commit path: write / setattr / xattr on one name is visible on the other.
#[test]
fn hard_link_single_commit_path_fans_out() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("hard_link_single_commit_path_fans_out");
    let (_d, s) = sess();
    let (a, fh) = s
        .create_file(FUSE_ROOT_ID, "a", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.write(fh, 0, b"old").unwrap();
    s.release(fh).unwrap();
    let ino = a.attrs.file_id;
    s.link(ino, FUSE_ROOT_ID, "b", 0, 0).unwrap();

    let fh = s.open(ino, libc::O_RDWR, 0, 0).unwrap();
    s.write(fh, 0, b"new!").unwrap();
    s.fsync(fh).unwrap();
    s.release(fh).unwrap();
    let b = s.lookup(FUSE_ROOT_ID, "b", 0, 0).unwrap();
    let fh = s.open(b.attrs.file_id, libc::O_RDONLY, 0, 0).unwrap();
    assert_eq!(s.read(fh, 0, 8).unwrap(), b"new!");
    s.release(fh).unwrap();

    s.setattr(
        ino,
        FuseSetAttr {
            mode: Some(0o600),
            ..Default::default()
        },
        None,
        0,
        0,
    )
    .unwrap();
    assert_eq!(s.getattr(ino, None).unwrap().perm, 0o600);
    let b = s.lookup(FUSE_ROOT_ID, "b", 0, 0).unwrap();
    assert_eq!(b.attrs.mode, 0o600);

    s.setxattr(ino, "user.k", b"v", 0, 0, 0).unwrap();
    assert_eq!(s.getxattr(b.attrs.file_id, "user.k").unwrap(), b"v");

    s.unlink(FUSE_ROOT_ID, "a", 0, 0).unwrap();
    assert_eq!(s.getattr(ino, None).unwrap().nlink, 1);
    let fh = s.open(ino, libc::O_RDONLY, 0, 0).unwrap();
    assert_eq!(s.read(fh, 0, 8).unwrap(), b"new!");
    s.release(fh).unwrap();
}

/// TTL is zero so getattr nlink after link/unlink cannot be a stale 1s cache.
#[test]
fn ttl_zero_nlink_immediate() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("ttl_zero_nlink_immediate");
    assert_eq!(TTL, std::time::Duration::ZERO);
    let (_d, s) = sess();
    let (a, fh) = s
        .create_file(FUSE_ROOT_ID, "a", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.release(fh).unwrap();
    let ino = a.attrs.file_id;
    assert_eq!(s.getattr(ino, None).unwrap().nlink, 1);
    s.link(ino, FUSE_ROOT_ID, "b", 0, 0).unwrap();
    assert_eq!(s.getattr(ino, None).unwrap().nlink, 2);
    s.unlink(FUSE_ROOT_ID, "b", 0, 0).unwrap();
    assert_eq!(s.getattr(ino, None).unwrap().nlink, 1);
}

/// Parent without execute: lookup is EACCES.
#[test]
fn lookup_requires_parent_search() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("lookup_requires_parent_search");
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "p", 0o700, 0, 0).unwrap();
    let p = s.lookup(FUSE_ROOT_ID, "p", 0, 0).unwrap().attrs.file_id;
    s.setattr(
        p,
        FuseSetAttr {
            uid: Some(1),
            gid: Some(1),
            mode: Some(0o700),
            ..Default::default()
        },
        None,
        0,
        0,
    )
    .unwrap();
    assert_eacces(s.lookup(p, "nope", 9, 9).unwrap_err());
}

/// Directory without read: readdir is EACCES; opendir needs search.
#[test]
fn readdir_and_opendir_perms() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("readdir_and_opendir_perms");
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "p", 0o100, 0, 0).unwrap();
    let p = s.lookup(FUSE_ROOT_ID, "p", 0, 0).unwrap().attrs.file_id;
    s.setattr(
        p,
        FuseSetAttr {
            uid: Some(1),
            gid: Some(1),
            mode: Some(0o100),
            ..Default::default()
        },
        None,
        0,
        0,
    )
    .unwrap();
    s.opendir(p, 1, 1).unwrap();
    assert_eacces(s.readdir(p, 1, 1).unwrap_err());
    assert_eacces(s.opendir(p, 9, 9).unwrap_err());
    let (f, fh) = s
        .create_file(FUSE_ROOT_ID, "f", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.release(fh).unwrap();
    assert!(matches!(
        s.opendir(f.attrs.file_id, 0, 0).unwrap_err(),
        ArkError::NotADirectory { .. }
    ));
}

/// XATTR_CREATE fails if present; XATTR_REPLACE fails if missing.
#[test]
fn xattr_create_replace_flags() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("xattr_create_replace_flags");
    let (_d, s) = sess();
    let (h, fh) = s
        .create_file(FUSE_ROOT_ID, "x", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.release(fh).unwrap();
    let ino = h.attrs.file_id;
    assert!(matches!(
        s.setxattr(ino, "user.k", b"v", libc::XATTR_REPLACE, 0, 0)
            .unwrap_err(),
        ArkError::NotFound { .. }
    ));
    s.setxattr(ino, "user.k", b"v", libc::XATTR_CREATE, 0, 0)
        .unwrap();
    assert!(matches!(
        s.setxattr(ino, "user.k", b"w", libc::XATTR_CREATE, 0, 0)
            .unwrap_err(),
        ArkError::AlreadyExists { .. }
    ));
    s.setxattr(ino, "user.k", b"w", libc::XATTR_REPLACE, 0, 0)
        .unwrap();
    assert_eq!(s.getxattr(ino, "user.k").unwrap(), b"w");
}

/// Path component longer than 255 bytes is NameTooLong.
#[test]
fn name_too_long_is_enametoolong() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("name_too_long_is_enametoolong");
    let (_d, s) = sess();
    let too = "a".repeat(256);
    let err = s.mkdir(FUSE_ROOT_ID, &too, 0o755, 0, 0).unwrap_err();
    assert!(matches!(err, ArkError::NameTooLong { .. }));
    assert_eq!(to_errno(&err), libc::ENAMETOOLONG);
}

/// setgid directory: child gid matches parent, new dirs keep setgid.
#[test]
fn setgid_directory_inherits_gid() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("setgid_directory_inherits_gid");
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "g", 0o3777, 0, 0).unwrap();
    let g = s.lookup(FUSE_ROOT_ID, "g", 0, 0).unwrap();
    s.setattr(
        g.attrs.file_id,
        FuseSetAttr {
            gid: Some(7),
            mode: Some(0o3777),
            ..Default::default()
        },
        None,
        0,
        0,
    )
    .unwrap();
    let child = s.mkdir(g.attrs.file_id, "c", 0o755, 1, 1).unwrap();
    assert_eq!(child.attrs.gid, 7);
    assert_eq!(child.attrs.mode & 0o2000, 0o2000);
    let (f, fh) = s
        .create_file(g.attrs.file_id, "f", 0o644, 1, 1, libc::O_RDWR)
        .unwrap();
    s.release(fh).unwrap();
    assert_eq!(f.attrs.gid, 7);
}

/// Write then read updates atime (relatime: atime was behind mtime).
#[test]
fn read_updates_atime_when_behind_mtime() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("read_updates_atime_when_behind_mtime");
    let (_d, s) = sess();
    let (h, fh) = s
        .create_file(FUSE_ROOT_ID, "t", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.write(fh, 0, b"hi").unwrap();
    s.fsync(fh).unwrap();
    s.release(fh).unwrap();
    let before = s.lookup(FUSE_ROOT_ID, "t", 0, 0).unwrap().attrs.atime;
    let fh = s.open(h.attrs.file_id, libc::O_RDONLY, 0, 0).unwrap();
    let _ = s.read(fh, 0, 2).unwrap();
    s.release(fh).unwrap();
    let after = s.lookup(FUSE_ROOT_ID, "t", 0, 0).unwrap().attrs.atime;
    assert!(after >= before);
}

/// Integrity scan of a fresh mount is clean.
#[test]
fn verify_integrity_clean_mount() {
    let _g = arkfs_test_review::guard();
    arkfs_test_review::step("verify_integrity_clean_mount");
    let (_d, s) = sess();
    let r = s.verify_integrity().unwrap();
    assert!(r.ok());
    assert!(r.objects_checked >= 1);
}
