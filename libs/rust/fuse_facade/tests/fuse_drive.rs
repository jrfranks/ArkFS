//! Live FUSE driver: kernel ops against a real mount, then inspect CAS on disk.
//!
//! GitHub (`CI` set, no `ARKFS_REQUIRE_FUSE`) skips if `/dev/fuse` is missing.
//! Local pre-commit sets `ARKFS_REQUIRE_FUSE=1` and fails without FUSE.
//!
//! Uses [`fuse_facade::spawn`] (not `mount`) so AutoUnmount/allow_other is not
//! required. After each op, [`fuse_facade::inspect`] asserts objects, tombstones,
//! and never-delete. Needs `fuse3` (`fusermount3`) and write access to `/dev/fuse`.

use fuse_facade::{inspect, spawn, ArkSession, DiskView};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

struct Harness {
    data: TempDir,
    mnt: TempDir,
    _bg: fuser::BackgroundSession,
}

impl Harness {
    fn new() -> Option<Self> {
        if !should_run_live() {
            return None;
        }
        let data = TempDir::new().unwrap();
        let mnt = TempDir::new().unwrap();
        let session = ArkSession::mount_store(data.path(), None).unwrap();
        let bg = spawn(session, mnt.path()).expect("spawn_mount2");
        wait_mounted(mnt.path());
        let h = Harness { data, mnt, _bg: bg };
        h.disk().assert_clean();
        h.disk().assert_dir("/");
        Some(h)
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.mnt.path().join(rel)
    }

    fn disk(&self) -> DiskView {
        unsafe { libc::sync() };
        inspect(self.data.path())
    }

    fn snap_objects(&self) -> std::collections::BTreeSet<String> {
        self.disk().object_hex
    }
}

fn should_run_live() -> bool {
    let present = Path::new("/dev/fuse").exists();
    if std::env::var_os("ARKFS_REQUIRE_FUSE").is_some() {
        assert!(present, "ARKFS_REQUIRE_FUSE=1 but /dev/fuse is missing");
        return true;
    }
    if std::env::var_os("CI").is_some() {
        eprintln!("skip live FUSE on GitHub CI");
        return false;
    }
    present
}

fn wait_mounted(mnt: &Path) {
    for _ in 0..100 {
        if fs::read_dir(mnt).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("FUSE mount did not appear at {}", mnt.display());
}

fn cstr(p: &Path) -> CString {
    CString::new(p.as_os_str().as_encoded_bytes()).unwrap()
}

/// One sequential drive of every FUSE op we implement, with a disk check after each.
#[test]
fn fuse_all_ops_then_disk() {
    let Some(h) = Harness::new() else {
        return;
    };
    let mut before = h.snap_objects();
    h.disk().assert_clean();

    // lookup + mkdir + getattr (stat)
    fs::create_dir(h.p("d")).unwrap();
    let meta = fs::metadata(h.p("d")).unwrap();
    assert!(meta.is_dir());
    let d = h.disk();
    d.assert_clean();
    d.assert_dir("/d");
    d.assert_never_deleted_objects(&before);
    before = d.object_hex;

    // create + write + flush + release
    {
        let mut f = File::create(h.p("d/a.txt")).unwrap();
        f.write_all(b"hello").unwrap();
        f.sync_all().unwrap();
    }
    let d = h.disk();
    d.assert_clean();
    d.assert_file("/d/a.txt", b"hello");
    d.assert_never_deleted_objects(&before);
    before = d.object_hex;

    // open + read
    let mut buf = String::new();
    File::open(h.p("d/a.txt"))
        .unwrap()
        .read_to_string(&mut buf)
        .unwrap();
    assert_eq!(buf, "hello");
    h.disk().assert_file("/d/a.txt", b"hello");

    // setattr: chmod, truncate
    fs::set_permissions(h.p("d/a.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    {
        let f = OpenOptions::new().write(true).open(h.p("d/a.txt")).unwrap();
        f.set_len(3).unwrap();
        f.sync_all().unwrap();
    }
    let d = h.disk();
    d.assert_file("/d/a.txt", b"hel");
    assert_eq!(d.live["/d/a.txt"].attrs.mode & 0o777, 0o640);
    d.assert_never_deleted_objects(&before);
    before = d.object_hex;

    // utimens (setattr atime/mtime)
    let t = filetime_now();
    set_times(h.p("d/a.txt"), t, t);
    h.disk().assert_file("/d/a.txt", b"hel");

    // write hole + fsync
    {
        let mut f = OpenOptions::new().write(true).open(h.p("d/a.txt")).unwrap();
        f.seek(SeekFrom::Start(8)).unwrap();
        f.write_all(b"Z").unwrap();
        f.sync_all().unwrap();
    }
    let d = h.disk();
    let mut expect = vec![0u8; 9];
    expect[..3].copy_from_slice(b"hel");
    expect[8] = b'Z';
    d.assert_file("/d/a.txt", &expect);
    d.assert_never_deleted_objects(&before);
    before = d.object_hex;

    // readdir / opendir / releasedir
    let names: Vec<_> = fs::read_dir(h.p("d"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, vec!["a.txt"]);

    // symlink + readlink
    symlink("a.txt", h.p("d/l")).unwrap();
    assert_eq!(fs::read_link(h.p("d/l")).unwrap(), Path::new("a.txt"));
    let d = h.disk();
    d.assert_symlink("/d/l", "a.txt");
    d.assert_never_deleted_objects(&before);
    before = d.object_hex;

    // rename
    fs::rename(h.p("d/a.txt"), h.p("d/b.txt")).unwrap();
    let d = h.disk();
    d.assert_file("/d/b.txt", &expect);
    d.assert_tombstone("/d/a.txt");
    d.assert_never_deleted_objects(&before);
    before = d.object_hex;

    // xattr
    xset(h.p("d/b.txt"), "user.k", b"v1");
    assert_eq!(xget(h.p("d/b.txt"), "user.k"), b"v1");
    assert!(xlist(h.p("d/b.txt")).iter().any(|n| n == "user.k"));
    xdel(h.p("d/b.txt"), "user.k");
    let d = h.disk();
    assert!(!d.live["/d/b.txt"].attrs.xattrs.contains_key("user.k"));
    d.assert_never_deleted_objects(&before);
    before = d.object_hex;

    // access
    let c = cstr(&h.p("d/b.txt"));
    assert_eq!(unsafe { libc::access(c.as_ptr(), libc::R_OK) }, 0);

    // statfs
    let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
    let cm = cstr(h.mnt.path());
    assert_eq!(unsafe { libc::statvfs(cm.as_ptr(), &mut vfs) }, 0);
    assert!(vfs.f_bsize > 0);

    // fsyncdir
    File::open(h.p("d")).unwrap().sync_all().unwrap();
    h.disk().assert_clean();

    // mknod regular
    let mp = cstr(&h.p("d/n.bin"));
    let rc = unsafe { libc::mknod(mp.as_ptr(), libc::S_IFREG | 0o600, 0) };
    assert_eq!(rc, 0, "mknod errno {}", std::io::Error::last_os_error());
    let d = h.disk();
    d.assert_file("/d/n.bin", b"");
    d.assert_never_deleted_objects(&before);
    before = d.object_hex;

    // fallocate / lseek
    {
        let f = OpenOptions::new().write(true).open(h.p("d/n.bin")).unwrap();
        let fd = f.as_raw_fd();
        let rc = unsafe { libc::fallocate(fd, 0, 0, 16) };
        if rc != 0 {
            // Kernel may not send FUSE_FALLOCATE; still grow the file.
            f.set_len(16).unwrap();
        }
        f.sync_all().unwrap();
        let hole = unsafe { libc::lseek(fd, 0, libc::SEEK_HOLE) };
        assert!(hole >= 0);
    }
    let d = h.disk();
    assert_eq!(d.live["/d/n.bin"].content.len(), 16);
    d.assert_never_deleted_objects(&before);
    before = d.object_hex;

    // getlk / setlk (fcntl)
    {
        let f = File::open(h.p("d/n.bin")).unwrap();
        let mut fl: libc::flock = unsafe { std::mem::zeroed() };
        fl.l_type = libc::F_RDLCK as i16;
        fl.l_whence = libc::SEEK_SET as i16;
        let rc = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETLK, &mut fl) };
        assert_eq!(rc, 0);
        fl.l_type = libc::F_UNLCK as i16;
        let rc = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETLK, &fl) };
        assert_eq!(rc, 0);
    }

    // ioctl → ENOTTY
    {
        let f = File::open(h.p("d/n.bin")).unwrap();
        let rc = unsafe { libc::ioctl(f.as_raw_fd(), 0x1234) };
        assert_eq!(rc, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOTTY)
        );
    }

    // link (hard) → EOPNOTSUPP
    let err = fs::hard_link(h.p("d/n.bin"), h.p("d/hard")).unwrap_err();
    let raw = err.raw_os_error();
    assert!(
        raw == Some(libc::EOPNOTSUPP) || raw == Some(libc::EPERM) || raw == Some(libc::ENOTSUP),
        "hard_link errno {err:?}"
    );
    h.disk().assert_clean();
    assert!(!h.disk().live.contains_key("/d/hard"));

    // unlink + rmdir + never-delete
    fs::remove_file(h.p("d/l")).unwrap();
    fs::remove_file(h.p("d/b.txt")).unwrap();
    fs::remove_file(h.p("d/n.bin")).unwrap();
    fs::remove_dir(h.p("d")).unwrap();
    let d = h.disk();
    d.assert_clean();
    d.assert_tombstone("/d/l");
    d.assert_tombstone("/d/b.txt");
    d.assert_tombstone("/d/n.bin");
    d.assert_tombstone("/d");
    d.assert_never_deleted_objects(&before);
    assert!(d.live.contains_key("/"));
}

fn filetime_now() -> libc::timespec {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) };
    ts
}

fn set_times(path: PathBuf, a: libc::timespec, m: libc::timespec) {
    let c = cstr(&path);
    let ts = [a, m];
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), ts.as_ptr(), 0) };
    assert_eq!(rc, 0, "utimensat {}", std::io::Error::last_os_error());
}

fn xset(path: PathBuf, name: &str, val: &[u8]) {
    let p = cstr(&path);
    let n = CString::new(name).unwrap();
    let rc = unsafe {
        libc::setxattr(
            p.as_ptr(),
            n.as_ptr(),
            val.as_ptr() as *const _,
            val.len(),
            0,
        )
    };
    assert_eq!(rc, 0, "setxattr {}", std::io::Error::last_os_error());
}

fn xget(path: PathBuf, name: &str) -> Vec<u8> {
    let p = cstr(&path);
    let n = CString::new(name).unwrap();
    let mut buf = vec![0u8; 256];
    let nread = unsafe {
        libc::getxattr(
            p.as_ptr(),
            n.as_ptr(),
            buf.as_mut_ptr() as *mut _,
            buf.len(),
        )
    };
    assert!(nread >= 0, "getxattr {}", std::io::Error::last_os_error());
    buf.truncate(nread as usize);
    buf
}

fn xlist(path: PathBuf) -> Vec<String> {
    let p = cstr(&path);
    let mut buf = vec![0u8; 256];
    let nread = unsafe { libc::listxattr(p.as_ptr(), buf.as_mut_ptr() as *mut _, buf.len()) };
    assert!(nread >= 0, "listxattr {}", std::io::Error::last_os_error());
    buf.truncate(nread as usize);
    buf.split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

fn xdel(path: PathBuf, name: &str) {
    let p = cstr(&path);
    let n = CString::new(name).unwrap();
    let rc = unsafe { libc::removexattr(p.as_ptr(), n.as_ptr()) };
    assert_eq!(rc, 0, "removexattr {}", std::io::Error::last_os_error());
}
