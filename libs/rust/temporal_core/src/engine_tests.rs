use super::*;
use arkfs_core::attr_map::{merge_from_fuse, FuseSetAttr};
use arkfs_core::{DosFlags, MacOsFlags, NamedStream, Timespec};
#[allow(unused_imports)]
use arkfs_test_review::{review_assert as assert, review_eq as assert_eq, review_ne as assert_ne};
use persistent_object_store::open_local_quorum_store;

/// Temp store with LocalQuorum n0/n1 and Quorum(1).
fn core() -> (tempfile::TempDir, TemporalCore) {
    let dir = tempfile::tempdir().unwrap();
    let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
    (dir, TemporalCore::open(store, QuorumPolicy::n(1)).unwrap())
}

/// commit_branch then lookup_current returns the bytes and size.
#[test]
fn commit_and_lookup_current() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    let t0 = Timestamp::new(1, 1000);
    tc.commit_branch(BranchDelta {
        path: "/notes.txt".into(),
        content: Some(b"v1".to_vec()),
        attrs: None,
        at: t0,
    })
    .unwrap();
    let h = tc.lookup_current("/notes.txt").unwrap();
    assert_eq!(tc.read_content(&h).unwrap(), b"v1");
    assert_eq!(h.attrs.logical_size, 2);
}

/// Second commit does not erase the first; AsOf still reads v1.
#[test]
fn historical_lookup_never_delete() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    let t0 = Timestamp::new(1, 1000);
    let t1 = Timestamp::new(2, 2000);
    tc.commit_branch(BranchDelta {
        path: "/f".into(),
        content: Some(b"old".to_vec()),
        attrs: None,
        at: t0,
    })
    .unwrap();
    tc.commit_branch(BranchDelta {
        path: "/f".into(),
        content: Some(b"new".to_vec()),
        attrs: None,
        at: t1,
    })
    .unwrap();
    assert_eq!(
        tc.read_content(&tc.lookup_at_timestamp("/f", t0).unwrap())
            .unwrap(),
        b"old"
    );
    assert_eq!(
        tc.read_content(&tc.lookup_at_timestamp("/f", t1).unwrap())
            .unwrap(),
        b"new"
    );
    assert_eq!(
        tc.read_content(&tc.lookup_current("/f").unwrap()).unwrap(),
        b"new"
    );
    assert_eq!(tc.parents("/f"), vec![None, Some(0)]);
}

/// setattr does not rewrite historical DOS/xattr bytes.
#[test]
fn attr_historical_snapshot() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    let mut a0 = FileAttributes::new_file(0, 0o644);
    a0.dos.hidden = true;
    a0.dos.archive = true;
    a0.xattrs
        .insert("com.apple.quarantine".into(), b"q".to_vec());
    let t0 = Timestamp::new(1, 1);
    tc.commit_branch(BranchDelta {
        path: "/a".into(),
        content: Some(b"x".to_vec()),
        attrs: Some(a0.clone()),
        at: t0,
    })
    .unwrap();

    let t1 = Timestamp::new(2, 2);
    tc.commit_attrs(
        "/a",
        |attrs| {
            attrs.dos.hidden = false;
            attrs.mode = 0o600;
        },
        t1,
    )
    .unwrap();

    let past = tc.lookup_at_timestamp("/a", t0).unwrap();
    assert!(past.attrs.dos.hidden);
    assert!(past.attrs.dos.archive);
    assert_eq!(past.attrs.mode, 0o644);
    assert!(past.attrs.xattrs.contains_key("com.apple.quarantine"));

    let cur = tc.lookup_current("/a").unwrap();
    assert!(!cur.attrs.dos.hidden);
    assert!(cur.attrs.dos.archive);
    assert_eq!(cur.attrs.mode, 0o600);
    assert!(cur.attrs.xattrs.contains_key("com.apple.quarantine"));
}

/// FUSE merge_from_fuse keeps SMB/macOS/dead props/streams.
#[test]
fn fuse_partial_does_not_clobber_smb_macos() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    let mut a = FileAttributes::new_file(0, 0o644);
    a.dos = DosFlags {
        hidden: true,
        system: true,
        archive: true,
        ..Default::default()
    };
    a.macos = MacOsFlags {
        uf_immutable: true,
        ..Default::default()
    };
    a.dead_props.insert("{urn:ex}color".into(), "blue".into());
    a.streams.push(NamedStream {
        name: NamedStream::RESOURCE_FORK.into(),
        size: 8,
        content_id: None,
    });
    let t0 = Timestamp::new(1, 1);
    tc.commit_branch(BranchDelta {
        path: "/m".into(),
        content: Some(b"data".to_vec()),
        attrs: Some(a),
        at: t0,
    })
    .unwrap();

    let t1 = Timestamp::new(2, 2);
    tc.commit_attrs(
        "/m",
        |attrs| {
            merge_from_fuse(
                attrs,
                &FuseSetAttr {
                    mode: Some(0o700),
                    uid: Some(99),
                    gid: None,
                    size: None,
                    atime: None,
                    mtime: None,
                    ctime: None,
                },
                Timespec::new(2, 0),
            );
        },
        t1,
    )
    .unwrap();

    let h = tc.lookup_current("/m").unwrap();
    assert_eq!(h.attrs.mode, 0o700);
    assert_eq!(h.attrs.uid, 99);
    assert!(h.attrs.dos.hidden);
    assert!(h.attrs.dos.system);
    assert!(h.attrs.dos.archive);
    assert!(h.attrs.macos.uf_immutable);
    assert_eq!(
        h.attrs.dead_props.get("{urn:ex}color").map(String::as_str),
        Some("blue")
    );
    assert!(h
        .attrs
        .streams
        .iter()
        .any(|s| s.name == NamedStream::RESOURCE_FORK));
}

/// Directory attrs: nlink 2, dos.directory, no streams.
#[test]
fn directory_roundtrip() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    let t0 = Timestamp::new(1, 1);
    tc.commit_branch(BranchDelta {
        path: "/dir".into(),
        content: Some(Vec::new()),
        attrs: Some(FileAttributes::new_dir(0, 0o755)),
        at: t0,
    })
    .unwrap();
    let h = tc.lookup_current("/dir").unwrap();
    assert_eq!(h.attrs.file_type, FileType::Directory);
    assert_eq!(h.attrs.nlink, 2);
    assert!(h.attrs.dos.directory);
    assert!(h.attrs.streams.is_empty());
    assert_eq!(h.attrs.mode, 0o755);
}

/// Commit with at <= last.at is Conflict and does not store the loser.
#[test]
fn conflict_before_write_keeps_latest() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.commit_branch(BranchDelta {
        path: "/c".into(),
        content: Some(b"new".to_vec()),
        attrs: None,
        at: Timestamp::new(2, 2),
    })
    .unwrap();
    let err = tc
        .commit_branch(BranchDelta {
            path: "/c".into(),
            content: Some(b"old".to_vec()),
            attrs: None,
            at: Timestamp::new(1, 1),
        })
        .unwrap_err();
    assert!(matches!(err, ArkError::Conflict { .. }));
    assert_eq!(
        tc.read_content(&tc.lookup_current("/c").unwrap()).unwrap(),
        b"new"
    );
}

/// Reopen from disk sees the latest content and cactus parents.
#[test]
fn index_survives_reopen() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    {
        let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
        let tc = TemporalCore::open(store, QuorumPolicy::n(1)).unwrap();
        tc.commit_branch(BranchDelta {
            path: "/p".into(),
            content: Some(b"persist".to_vec()),
            attrs: None,
            at: Timestamp::new(1, 1),
        })
        .unwrap();
        tc.commit_branch(BranchDelta {
            path: "/p".into(),
            content: Some(b"persist2".to_vec()),
            attrs: None,
            at: Timestamp::new(2, 2),
        })
        .unwrap();
    }
    let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
    let tc = TemporalCore::open(store, QuorumPolicy::n(1)).unwrap();
    let h = tc.lookup_current("/p").unwrap();
    assert_eq!(tc.read_content(&h).unwrap(), b"persist2");
    assert_eq!(h.attrs.file_id, 1);
    assert_eq!(tc.parents("/p"), vec![None, Some(0)]);
}

/// mkdir/create/readdir/unlink; AsOf still reads unlinked bytes; root is ino 1.
#[test]
fn mkdir_readdir_unlink_as_of() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    tc.mkdir("/", "d", 0o755, 0, 0).unwrap();
    tc.create_file("/d", "f.txt", 0o644, 0, 0).unwrap();
    let written = tc.replace_content("/d/f.txt", b"hello".to_vec()).unwrap();
    let names: Vec<_> = tc
        .readdir("/d", View::Live)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, vec!["f.txt"]);
    let before = written.committed_at;
    tc.unlink("/d/f.txt").unwrap();
    assert!(tc.lookup_current("/d/f.txt").is_err());
    assert!(tc.readdir("/d", View::Live).unwrap().is_empty());
    let past = tc.lookup_at_timestamp("/d/f.txt", before).unwrap();
    assert_eq!(tc.read_content(&past).unwrap(), b"hello");
    let ino_root = tc.lookup_current("/").unwrap().attrs.file_id;
    assert_eq!(ino_root, 1);
    let h = tc.lookup_ino(ino_root, View::Live).unwrap();
    assert!(h.path.is_root());
}

/// File rename then unlink+rmdir; old name is gone.
#[test]
fn rename_and_rmdir() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    tc.mkdir("/", "a", 0o755, 0, 0).unwrap();
    tc.create_file("/a", "x", 0o644, 0, 0).unwrap();
    tc.replace_content("/a/x", b"z".to_vec()).unwrap();
    tc.rename("/a/x", "/a", "y", 0).unwrap();
    assert!(tc.lookup_current("/a/x").is_err());
    assert_eq!(
        tc.read_content(&tc.lookup_current("/a/y").unwrap())
            .unwrap(),
        b"z"
    );
    tc.unlink("/a/y").unwrap();
    tc.rmdir("/a").unwrap();
    assert!(tc.lookup_current("/a").is_err());
}

/// Second create of a live name is AlreadyExists.
#[test]
fn create_rejects_existing() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    tc.create_file("/", "e", 0o644, 0, 0).unwrap();
    let err = tc.create_file("/", "e", 0o644, 0, 0).unwrap_err();
    assert!(matches!(err, ArkError::AlreadyExists { .. }));
}

/// Recreate after tombstone allocates a new file_id, never 0.
#[test]
fn unlink_then_create_gets_new_nonzero_ino() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    let first = tc.create_file("/", "e", 0o644, 0, 0).unwrap();
    assert_ne!(first.attrs.file_id, 0);
    tc.unlink("/e").unwrap();
    let second = tc.create_file("/", "e", 0o644, 0, 0).unwrap();
    assert_ne!(second.attrs.file_id, 0);
    assert_ne!(second.attrs.file_id, first.attrs.file_id);
    assert_ne!(second.attrs.file_id, 1);
}

/// Renaming /a to /b moves /a/x and /a/sub/y in one persist.
#[test]
fn directory_rename_moves_children() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    tc.mkdir("/", "a", 0o755, 0, 0).unwrap();
    tc.create_file("/a", "x", 0o644, 0, 0).unwrap();
    tc.replace_content("/a/x", b"z".to_vec()).unwrap();
    tc.mkdir("/a", "sub", 0o755, 0, 0).unwrap();
    tc.create_file("/a/sub", "y", 0o644, 0, 0).unwrap();
    tc.replace_content("/a/sub/y", b"yy".to_vec()).unwrap();
    let dir_ino = tc.lookup_current("/a").unwrap().attrs.file_id;
    tc.rename("/a", "/", "b", 0).unwrap();
    assert!(tc.lookup_current("/a").is_err());
    assert!(tc.lookup_current("/a/x").is_err());
    assert_eq!(
        tc.read_content(&tc.lookup_current("/b/x").unwrap())
            .unwrap(),
        b"z"
    );
    assert_eq!(
        tc.read_content(&tc.lookup_current("/b/sub/y").unwrap())
            .unwrap(),
        b"yy"
    );
    assert_eq!(tc.lookup_current("/b").unwrap().attrs.file_id, dir_ino);
    let names: Vec<_> = tc
        .readdir("/b", View::Live)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, vec!["sub", "x"]);
}

/// link shares file_id; nlink 2 then 1 after unlink of one name.
#[test]
fn hard_link_shares_inode_and_nlink() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    let a = tc.create_file("/", "a", 0o644, 0, 0).unwrap();
    tc.replace_content("/a", b"hi".to_vec()).unwrap();
    let b = tc.link("/a", "/", "b").unwrap();
    assert_eq!(a.attrs.file_id, b.attrs.file_id);
    assert_eq!(tc.lookup_current("/a").unwrap().attrs.nlink, 2);
    assert_eq!(tc.lookup_current("/b").unwrap().attrs.nlink, 2);
    assert_eq!(
        tc.read_content(&tc.lookup_current("/b").unwrap()).unwrap(),
        b"hi"
    );
    tc.unlink("/a").unwrap();
    assert_eq!(tc.lookup_current("/b").unwrap().attrs.nlink, 1);
    assert_eq!(
        tc.read_content(&tc.lookup_current("/b").unwrap()).unwrap(),
        b"hi"
    );
}

/// mknod fifo and char device with rdev.
#[test]
fn mknod_fifo_and_device() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    let f = tc
        .mknod("/", "p", FileType::Fifo, 0o644, 0, 0, None)
        .unwrap();
    assert_eq!(f.attrs.file_type, FileType::Fifo);
    let d = tc
        .mknod("/", "c", FileType::CharDevice, 0o600, 0, 0, Some(0x0103))
        .unwrap();
    assert_eq!(d.attrs.file_type, FileType::CharDevice);
    assert_eq!(d.attrs.rdev, Some(0x0103));
}

/// replace_content on one hard-link name is visible on the other.
#[test]
fn hard_link_write_visible_on_other_name() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    tc.create_file("/", "a", 0o644, 0, 0).unwrap();
    tc.replace_content("/a", b"old".to_vec()).unwrap();
    tc.link("/a", "/", "b").unwrap();
    tc.replace_content("/a", b"new".to_vec()).unwrap();
    assert_eq!(
        tc.read_content(&tc.lookup_current("/b").unwrap()).unwrap(),
        b"new"
    );
    assert_eq!(
        tc.read_content(&tc.lookup_current("/a").unwrap()).unwrap(),
        b"new"
    );
}

/// RENAME_NOREPLACE is EEXIST; RENAME_EXCHANGE swaps file bytes.
#[test]
fn rename_noreplace_and_exchange() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    tc.create_file("/", "a", 0o644, 0, 0).unwrap();
    tc.replace_content("/a", b"A".to_vec()).unwrap();
    tc.create_file("/", "b", 0o644, 0, 0).unwrap();
    tc.replace_content("/b", b"B".to_vec()).unwrap();
    let err = tc.rename("/a", "/", "b", RENAME_NOREPLACE).unwrap_err();
    assert!(matches!(err, ArkError::AlreadyExists { .. }));
    tc.rename("/a", "/", "b", RENAME_EXCHANGE).unwrap();
    assert_eq!(
        tc.read_content(&tc.lookup_current("/a").unwrap()).unwrap(),
        b"B"
    );
    assert_eq!(
        tc.read_content(&tc.lookup_current("/b").unwrap()).unwrap(),
        b"A"
    );
}

/// RENAME_EXCHANGE of two dirs swaps their child trees.
#[test]
fn directory_exchange_swaps_children() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    tc.mkdir("/", "a", 0o755, 0, 0).unwrap();
    tc.mkdir("/", "b", 0o755, 0, 0).unwrap();
    tc.create_file("/a", "x", 0o644, 0, 0).unwrap();
    tc.replace_content("/a/x", b"ax".to_vec()).unwrap();
    tc.create_file("/b", "y", 0o644, 0, 0).unwrap();
    tc.replace_content("/b/y", b"by".to_vec()).unwrap();
    tc.rename("/a", "/", "b", RENAME_EXCHANGE).unwrap();
    assert_eq!(
        tc.read_content(&tc.lookup_current("/b/x").unwrap())
            .unwrap(),
        b"ax"
    );
    assert_eq!(
        tc.read_content(&tc.lookup_current("/a/y").unwrap())
            .unwrap(),
        b"by"
    );
    assert!(tc.lookup_current("/a/x").is_err());
    assert!(tc.lookup_current("/b/y").is_err());
}

/// POSIX error edges: rmdir root/nonempty/file, unlink dir, link dir, rename into self.
#[test]
fn posix_op_error_boundaries() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    assert!(matches!(
        tc.rmdir("/").unwrap_err(),
        ArkError::InvalidArgument { .. }
    ));
    assert!(matches!(
        tc.lookup_current("/missing").unwrap_err(),
        ArkError::NotFound { .. }
    ));
    tc.mkdir("/", "a", 0o755, 0, 0).unwrap();
    tc.create_file("/a", "f", 0o644, 0, 0).unwrap();
    assert!(matches!(
        tc.unlink("/a").unwrap_err(),
        ArkError::IsADirectory { .. }
    ));
    assert!(matches!(
        tc.rmdir("/a/f").unwrap_err(),
        ArkError::NotADirectory { .. }
    ));
    assert!(matches!(
        tc.rmdir("/a").unwrap_err(),
        ArkError::NotEmpty { .. }
    ));
    assert!(matches!(
        tc.readdir("/a/f", View::Live).unwrap_err(),
        ArkError::NotADirectory { .. }
    ));
    assert!(matches!(
        tc.link("/a", "/", "l").unwrap_err(),
        ArkError::IsADirectory { .. }
    ));
    assert!(matches!(
        tc.rename("/a", "/a", "f", 0).unwrap_err(),
        ArkError::InvalidArgument { .. }
    ));
    assert!(matches!(
        tc.rename("/a/f", "/", "a", 0).unwrap_err(),
        ArkError::IsADirectory { .. }
    ));
    let same = tc.rename("/a/f", "/a", "f", 0).unwrap();
    assert_eq!(same.path.as_str(), "/a/f");
    assert!(matches!(
        tc.replace_content("/a", vec![1]).unwrap_err(),
        ArkError::IsADirectory { .. }
    ));
    tc.mknod("/", "p", FileType::Fifo, 0o644, 0, 0, None)
        .unwrap();
    assert!(matches!(
        tc.replace_content("/p", vec![1]).unwrap_err(),
        ArkError::InvalidArgument { .. }
    ));
    assert!(matches!(
        tc.rename("/a/f", "/", "g", 1 | 2).unwrap_err(),
        ArkError::InvalidArgument { .. }
    ));
    assert!(matches!(
        tc.rename("/a/f", "/", "g", 4).unwrap_err(),
        ArkError::InvalidArgument { .. }
    ));
    let dir = tc
        .mknod("/", "via_mknod", FileType::Directory, 0o700, 0, 0, None)
        .unwrap();
    assert_eq!(dir.attrs.file_type, FileType::Directory);
}

/// Live lookup_ino uses the inode map; a tombstone is missing live and present AsOf.
#[test]
fn lookup_ino_live_map_and_as_of_scan() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    let a = tc.create_file("/", "a", 0o644, 0, 0).unwrap();
    let ino = a.attrs.file_id;
    assert_eq!(tc.lookup_ino(ino, View::Live).unwrap().path.as_str(), "/a");
    let at = a.committed_at;
    tc.unlink("/a").unwrap();
    assert!(tc.lookup_ino(ino, View::Live).is_err());
    assert_eq!(
        tc.lookup_ino(ino, View::AsOf(at)).unwrap().path.as_str(),
        "/a"
    );
}

/// Hard links: live map keeps the lexicographically first path (same as find_ino).
#[test]
fn lookup_ino_hard_link_is_lex_first_path() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    let z = tc.create_file("/", "z", 0o644, 0, 0).unwrap();
    let ino = z.attrs.file_id;
    tc.link("/z", "/", "a").unwrap();
    assert_eq!(tc.lookup_ino(ino, View::Live).unwrap().path.as_str(), "/a");
    tc.rename("/z", "/", "m", 0).unwrap();
    assert_eq!(tc.lookup_ino(ino, View::Live).unwrap().path.as_str(), "/a");
}

/// Reopen rebuilds the live inode map from the durable index.
#[test]
fn lookup_ino_map_survives_reopen() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let ino;
    {
        let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
        let tc = TemporalCore::open(store, QuorumPolicy::n(1)).unwrap();
        tc.ensure_root().unwrap();
        let h = tc.create_file("/", "p", 0o644, 0, 0).unwrap();
        ino = h.attrs.file_id;
        tc.rename("/p", "/", "q", 0).unwrap();
    }
    let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
    let tc = TemporalCore::open(store, QuorumPolicy::n(1)).unwrap();
    assert_eq!(tc.lookup_ino(ino, View::Live).unwrap().path.as_str(), "/q");
}

/// commit_attrs on one hard-link name is visible on the other (single commit path).
#[test]
fn commit_attrs_fans_out_to_hard_link() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    tc.create_file("/", "a", 0o644, 0, 0).unwrap();
    tc.link("/a", "/", "b").unwrap();
    tc.commit_attrs_now("/a", |attrs| attrs.mode = 0o600)
        .unwrap();
    assert_eq!(tc.lookup_current("/a").unwrap().attrs.mode, 0o600);
    assert_eq!(tc.lookup_current("/b").unwrap().attrs.mode, 0o600);
}

/// live_path_count counts root plus live names, not tombstones.
#[test]
fn live_path_count_skips_tombstones() {
    let _g = arkfs_test_review::guard();
    let (_d, tc) = core();
    tc.ensure_root().unwrap();
    let n = tc.live_path_count();
    tc.create_file("/", "a", 0o644, 0, 0).unwrap();
    assert_eq!(tc.live_path_count(), n + 1);
    tc.unlink("/a").unwrap();
    assert_eq!(tc.live_path_count(), n);
}
