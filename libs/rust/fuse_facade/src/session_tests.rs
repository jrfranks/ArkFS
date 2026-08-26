//! In-process [`ArkSession`] tests (no `/dev/fuse`). Live kernel coverage is `fuse_drive.rs`.

use super::*;
#[allow(unused_imports)]
use arkfs_test_review::{review_assert as assert, review_eq as assert_eq, review_ne as assert_ne};
use tempfile::tempdir;

/// Fresh isolated store + live ArkSession (no /dev/fuse).
fn sess() -> (tempfile::TempDir, ArkSession) {
    let d = tempdir().unwrap();
    let s = ArkSession::mount_store(d.path(), None).unwrap();
    (d, s)
}

/// End-to-end mkdir/create/write/fsync/read/unlink on ArkSession.
#[test]
fn mkdir_create_write_read_unlink() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "w", 0o755, 0, 0).unwrap();
    let dir = s.lookup(FUSE_ROOT_ID, "w", 0, 0).unwrap();
    let (f, fh) = s
        .create_file(dir.attrs.file_id, "a.txt", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    assert_eq!(s.write(fh, 0, b"abc").unwrap(), 3);
    s.fsync(fh).unwrap();
    s.release(fh).unwrap();
    let fh = s.open(f.attrs.file_id, libc::O_RDONLY, 0, 0).unwrap();
    assert_eq!(s.read(fh, 0, 10).unwrap(), b"abc");
    s.release(fh).unwrap();
    s.unlink(dir.attrs.file_id, "a.txt", 0, 0).unwrap();
    assert!(s.lookup(dir.attrs.file_id, "a.txt", 0, 0).is_err());
    let names: Vec<_> = s
        .readdir(dir.attrs.file_id, 0, 0)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert!(names.is_empty());
}

/// Single-node mount must not create data/replicas/.
#[test]
fn isolated_mount_has_no_replicas_dir() {
    let _g = arkfs_test_review::guard();
    let (d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "x", 0o755, 0, 0).unwrap();
    assert!(!d.path().join("replicas").exists());
    assert!(d.path().join("primary/objects").is_dir());
}

/// Two fhs on one inode see each other's unflushed writes.
#[test]
fn two_fhs_share_inode_buffer() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    let (f, fh1) = s
        .create_file(FUSE_ROOT_ID, "s", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.write(fh1, 0, b"abc").unwrap();
    let fh2 = s.open(f.attrs.file_id, libc::O_RDWR, 0, 0).unwrap();
    assert_eq!(s.read(fh2, 0, 3).unwrap(), b"abc");
    s.write(fh2, 0, b"XYZ").unwrap();
    assert_eq!(s.read(fh1, 0, 3).unwrap(), b"XYZ");
    s.release(fh1).unwrap();
    s.release(fh2).unwrap();
}

/// O_TRUNC zeros the inode for every fh and persists empty.
#[test]
fn o_trunc_is_inode_wide_and_durable() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    let (f, fh) = s
        .create_file(FUSE_ROOT_ID, "t", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.write(fh, 0, b"hello").unwrap();
    s.release(fh).unwrap();
    let fh = s
        .open(f.attrs.file_id, libc::O_RDWR | libc::O_TRUNC, 0, 0)
        .unwrap();
    assert_eq!(s.getattr(f.attrs.file_id, None).unwrap().size, 0);
    s.release(fh).unwrap();
    let fh = s.open(f.attrs.file_id, libc::O_RDONLY, 0, 0).unwrap();
    assert!(s.read(fh, 0, 8).unwrap().is_empty());
    s.release(fh).unwrap();
}

/// Recreate after unlink gets a new non-zero file_id (not inode 0 or 1).
#[test]
fn unlink_recreate_new_ino() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    let (f, fh) = s
        .create_file(FUSE_ROOT_ID, "e", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.release(fh).unwrap();
    let old = f.attrs.file_id;
    assert_ne!(old, 0);
    s.unlink(FUSE_ROOT_ID, "e", 0, 0).unwrap();
    let (f2, fh) = s
        .create_file(FUSE_ROOT_ID, "e", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.release(fh).unwrap();
    assert_ne!(f2.attrs.file_id, 0);
    assert_ne!(f2.attrs.file_id, old);
    let a = s.getattr(f2.attrs.file_id, None).unwrap();
    assert_eq!(a.ino, f2.attrs.file_id);
}

/// --as-of mount rejects mkdir.
#[test]
fn as_of_is_read_only() {
    let _g = arkfs_test_review::guard();
    let d = tempdir().unwrap();
    let s = ArkSession::mount_store(d.path(), None).unwrap();
    s.mkdir(FUSE_ROOT_ID, "d", 0o755, 0, 0).unwrap();
    drop(s);
    let s = ArkSession::mount_store(d.path(), Some(1)).unwrap();
    assert!(s.mkdir(FUSE_ROOT_ID, "nope", 0o755, 0, 0).is_err());
}

/// chmod via setattr persists permission bits.
#[test]
fn setattr_mode() {
    let _g = arkfs_test_review::guard();
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
        0,
        0,
    )
    .unwrap();
    let a = s.getattr(f.attrs.file_id, None).unwrap();
    assert_eq!(a.perm, 0o600);
}

/// Renaming a dir moves children; an open fh still reads after the move.
#[test]
fn directory_rename_moves_open_and_closed_children() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "a", 0o755, 0, 0).unwrap();
    let a = s.lookup(FUSE_ROOT_ID, "a", 0, 0).unwrap().attrs.file_id;
    s.mkdir(a, "sub", 0o755, 0, 0).unwrap();
    let sub = s.lookup(a, "sub", 0, 0).unwrap().attrs.file_id;
    let (f, fh) = s.create_file(sub, "x", 0o644, 0, 0, libc::O_RDWR).unwrap();
    s.write(fh, 0, b"z").unwrap();
    s.rename(FUSE_ROOT_ID, "a", FUSE_ROOT_ID, "b", 0, 0, 0)
        .unwrap();
    assert!(s.lookup(FUSE_ROOT_ID, "a", 0, 0).is_err());
    let b = s.lookup(FUSE_ROOT_ID, "b", 0, 0).unwrap();
    let names: Vec<_> = s
        .readdir(b.attrs.file_id, 0, 0)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, vec!["sub"]);
    assert_eq!(s.read(fh, 0, 1).unwrap(), b"z");
    s.release(fh).unwrap();
    let x = s.lookup(sub, "x", 0, 0).unwrap();
    assert_eq!(x.attrs.file_id, f.attrs.file_id);
    let fh = s.open(x.attrs.file_id, libc::O_RDONLY, 0, 0).unwrap();
    assert_eq!(s.read(fh, 0, 1).unwrap(), b"z");
    s.release(fh).unwrap();
}

/// Hard link shares file_id/nlink; copy_file_range copies bytes to a new file.
#[test]
fn hard_link_and_copy_file_range() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    let (a, fh) = s
        .create_file(FUSE_ROOT_ID, "a", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.write(fh, 0, b"hello").unwrap();
    s.release(fh).unwrap();
    let b = s.link(a.attrs.file_id, FUSE_ROOT_ID, "b", 0, 0).unwrap();
    assert_eq!(a.attrs.file_id, b.attrs.file_id);
    assert_eq!(s.getattr(a.attrs.file_id, None).unwrap().nlink, 2);
    let (c, fhc) = s
        .create_file(FUSE_ROOT_ID, "c", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    let fha = s.open(a.attrs.file_id, libc::O_RDONLY, 0, 0).unwrap();
    assert_eq!(s.copy_file_range(fha, 0, fhc, 0, 5, 0).unwrap(), 5);
    s.release(fha).unwrap();
    s.release(fhc).unwrap();
    let fh = s.open(c.attrs.file_id, libc::O_RDONLY, 0, 0).unwrap();
    assert_eq!(s.read(fh, 0, 5).unwrap(), b"hello");
    s.release(fh).unwrap();
}

/// mknod fifo, fcntl lock conflict, poll ready, identity bmap, readdirplus.
#[test]
fn mknod_fifo_locks_poll_bmap() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    let p = s
        .mknod(FUSE_ROOT_ID, "p", libc::S_IFIFO | 0o644, 0, 0, 0)
        .unwrap();
    assert_eq!(p.attrs.file_type, FileType::Fifo);
    let (f, fh) = s
        .create_file(FUSE_ROOT_ID, "f", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    let ino = f.attrs.file_id;
    s.setlk(ino, 1, 0, 9, libc::F_WRLCK, 7, false).unwrap();
    let (_s, _e, typ, pid) = s.getlk(ino, 2, 0, 9, libc::F_RDLCK, 8).unwrap();
    assert_eq!(typ, libc::F_WRLCK);
    assert_eq!(pid, 7);
    s.setlk(ino, 1, 0, 9, libc::F_UNLCK, 7, false).unwrap();
    let ready = s.poll(ino, Some(fh), libc::POLLIN as u32).unwrap();
    assert_ne!(ready & libc::POLLIN as u32, 0);
    assert_eq!(s.bmap(ino, 512, 3).unwrap(), 3);
    s.release(fh).unwrap();
    let plus = s.readdir_plus(FUSE_ROOT_ID, 0, 0).unwrap();
    assert!(plus.iter().any(|(e, _, _)| e.name == "p"));
    assert!(plus.iter().any(|(e, a, _)| e.name == "f" && a.ino == ino));
}

/// copy_file_range: flags, negative, len 0, short copy, same-file overlap, dest O_RDONLY.
#[test]
fn copy_file_range_boundaries() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    let (h, fh) = s
        .create_file(FUSE_ROOT_ID, "c", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.write(fh, 0, b"abcdef").unwrap();
    assert_eq!(s.copy_file_range(fh, 0, fh, 0, 0, 0).unwrap(), 0);
    assert!(s.copy_file_range(fh, 0, fh, 0, 1, 1).is_err());
    assert!(s.copy_file_range(fh, -1, fh, 0, 1, 0).is_err());
    assert!(s.copy_file_range(fh, 0, fh, -1, 1, 0).is_err());
    assert_eq!(s.copy_file_range(fh, 0, fh, 0, 100, 0).unwrap(), 6);
    assert_eq!(s.copy_file_range(fh, 0, fh, 2, 4, 0).unwrap(), 4);
    s.fsync(fh).unwrap();
    assert_eq!(s.read(fh, 0, 16).unwrap(), b"ababcd");
    s.release(fh).unwrap();
    let ro = s.open(h.attrs.file_id, libc::O_RDONLY, 0, 0).unwrap();
    let (_d2, fhw) = s
        .create_file(FUSE_ROOT_ID, "w", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    assert!(s.copy_file_range(fhw, 0, ro, 0, 1, 0).is_err());
    s.release(ro).unwrap();
    s.release(fhw).unwrap();
}

/// open dir, write fifo, lock dir, poll 0, lseek/fallocate invalid, rename into child.
#[test]
fn session_op_error_boundaries() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    assert!(matches!(
        s.open(FUSE_ROOT_ID, libc::O_RDONLY, 0, 0).unwrap_err(),
        ArkError::IsADirectory { .. }
    ));
    s.mkdir(FUSE_ROOT_ID, "a", 0o755, 0, 0).unwrap();
    let a = s.lookup(FUSE_ROOT_ID, "a", 0, 0).unwrap().attrs.file_id;
    s.mkdir(a, "b", 0o755, 0, 0).unwrap();
    assert!(matches!(
        s.rename(FUSE_ROOT_ID, "a", a, "b", 0, 0, 0).unwrap_err(),
        ArkError::InvalidArgument { .. }
    ));
    assert!(matches!(
        s.link(a, FUSE_ROOT_ID, "l", 0, 0).unwrap_err(),
        ArkError::IsADirectory { .. }
    ));
    assert!(matches!(
        s.setlk(FUSE_ROOT_ID, 1, 0, 1, libc::F_WRLCK, 1, false)
            .unwrap_err(),
        ArkError::InvalidArgument { .. }
    ));
    let p = s
        .mknod(FUSE_ROOT_ID, "p", libc::S_IFIFO | 0o644, 0, 0, 0)
        .unwrap();
    assert!(matches!(
        s.open(p.attrs.file_id, libc::O_RDWR, 0, 0).unwrap_err(),
        ArkError::NoSuchDevice { .. }
    ));
    let (f, fh) = s
        .create_file(FUSE_ROOT_ID, "f", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    let ino = f.attrs.file_id;
    assert_eq!(s.poll(ino, Some(fh), 0).unwrap(), 0);
    assert!(s.lseek(ino, Some(fh), 0, 99).is_err());
    assert!(s.lseek(ino, Some(fh), -1, libc::SEEK_SET).is_err());
    assert_eq!(s.lseek(ino, Some(fh), 0, libc::SEEK_HOLE).unwrap(), 0);
    assert!(s.lseek(ino, Some(fh), 0, libc::SEEK_DATA).is_err());
    s.write(fh, 0, b"xy").unwrap();
    assert_eq!(s.lseek(ino, Some(fh), 0, libc::SEEK_DATA).unwrap(), 0);
    assert_eq!(s.lseek(ino, Some(fh), 0, libc::SEEK_HOLE).unwrap(), 2);
    assert!(s.fallocate(ino, Some(fh), -1, 1, 0).is_err());
    assert!(s.fallocate(ino, Some(fh), 0, -1, 0).is_err());
    assert!(s.fallocate(ino, Some(fh), 0, 1, 99).is_err());
    let before = s.getattr(ino, Some(fh)).unwrap().size;
    s.fallocate(ino, Some(fh), 0, 8, libc::FALLOC_FL_KEEP_SIZE)
        .unwrap();
    assert_eq!(s.getattr(ino, Some(fh)).unwrap().size, before);
    s.release(fh).unwrap();
}

/// Unix access: F_OK, other-read on 0644, other-write denied, root write allowed.
#[test]
fn access_unix_bits_and_root_bypass() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    let (f, fh) = s
        .create_file(FUSE_ROOT_ID, "a", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.release(fh).unwrap();
    let ino = f.attrs.file_id;
    s.access(ino, libc::F_OK, 9, 9).unwrap();
    s.access(ino, libc::R_OK, 9, 9).unwrap();
    assert!(matches!(
        s.access(ino, libc::W_OK, 9, 9).unwrap_err(),
        ArkError::PermissionDenied { .. }
    ));
    s.access(ino, libc::W_OK, 0, 0).unwrap();
    assert!(matches!(
        s.open(ino, libc::O_WRONLY, 9, 9).unwrap_err(),
        ArkError::PermissionDenied { .. }
    ));
}

/// Non-root cannot create in a 0700 directory owned by someone else.
#[test]
fn mkdir_create_denied_without_parent_write() {
    let _g = arkfs_test_review::guard();
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
    assert!(matches!(
        s.mkdir(p, "c", 0o755, 9, 9).unwrap_err(),
        ArkError::PermissionDenied { .. }
    ));
    assert!(matches!(
        s.create_file(p, "f", 0o644, 9, 9, libc::O_RDWR)
            .unwrap_err(),
        ArkError::PermissionDenied { .. }
    ));
    s.mkdir(p, "ok", 0o755, 1, 1).unwrap();
}

/// Sticky bit: other cannot unlink a file they do not own.
#[test]
fn sticky_dir_blocks_foreign_unlink() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "t", 0o1777, 0, 0).unwrap();
    let t = s.lookup(FUSE_ROOT_ID, "t", 0, 0).unwrap().attrs.file_id;
    let (_f, fh) = s.create_file(t, "x", 0o644, 1, 1, libc::O_RDWR).unwrap();
    s.release(fh).unwrap();
    assert!(matches!(
        s.unlink(t, "x", 9, 9).unwrap_err(),
        ArkError::PermissionDenied { .. }
    ));
    s.unlink(t, "x", 1, 1).unwrap();
}

/// chmod/chown as non-owner is EACCES; owner can chmod.
#[test]
fn setattr_owner_and_root() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    s.mkdir(FUSE_ROOT_ID, "w", 0o777, 0, 0).unwrap();
    let w = s.lookup(FUSE_ROOT_ID, "w", 0, 0).unwrap().attrs.file_id;
    let (f, fh) = s.create_file(w, "m", 0o644, 5, 5, libc::O_RDWR).unwrap();
    s.release(fh).unwrap();
    let ino = f.attrs.file_id;
    assert!(matches!(
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
        ArkError::PermissionDenied { .. }
    ));
    assert!(matches!(
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
        ArkError::PermissionDenied { .. }
    ));
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
    assert_eq!(s.getattr(ino, None).unwrap().perm, 0o600);
}

/// fifo / char / block / socket open is ENXIO, not EINVAL.
#[test]
fn special_file_open_is_enxio() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    for (name, mode) in [
        ("p", libc::S_IFIFO | 0o644),
        ("c", libc::S_IFCHR | 0o600),
        ("b", libc::S_IFBLK | 0o600),
        ("s", libc::S_IFSOCK | 0o644),
    ] {
        let h = s.mknod(FUSE_ROOT_ID, name, mode, 0, 0, 0).unwrap();
        assert!(
            matches!(
                s.open(h.attrs.file_id, libc::O_RDWR, 0, 0).unwrap_err(),
                ArkError::NoSuchDevice { .. }
            ),
            "{name}"
        );
    }
}

/// statfs reports 4096-byte blocks, 255-byte names, and live file count.
#[test]
fn statfs_tracks_live_paths() {
    let _g = arkfs_test_review::guard();
    let (_d, s) = sess();
    let before = s.statfs().unwrap();
    assert_eq!(before.bsize, 4096);
    assert_eq!(before.frsize, 4096);
    assert_eq!(before.namelen, 255);
    assert!(before.files >= 1);
    assert!(before.blocks >= 1);
    let (_f, fh) = s
        .create_file(FUSE_ROOT_ID, "n", 0o644, 0, 0, libc::O_RDWR)
        .unwrap();
    s.release(fh).unwrap();
    let after = s.statfs().unwrap();
    assert!(after.files > before.files);
}

/// Kernel attr TTL is zero so nlink is not cached across link/unlink.
#[test]
fn attr_ttl_is_zero() {
    let _g = arkfs_test_review::guard();
    assert_eq!(TTL, std::time::Duration::ZERO);
}
