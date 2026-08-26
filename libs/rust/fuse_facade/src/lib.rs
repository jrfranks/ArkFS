//! FUSE facade over TemporalCore. Filesystem semantics live in TemporalCore;
//! this crate translates inodes, open-file caches, and errno.
//!
//! Linux only (`fuser` 0.15, `default-features = false`, `abi-7-31`).
//!
//! | Module | Job |
//! |--------|-----|
//! | `session` | In-process node: POSIX via TemporalCore + open-file `Vec` cache |
//! | `fuse` | `fuser::Filesystem` impl + `mount` / `spawn` |
//! | `err` | Total `ArkError` → errno map (adding a variant is a compile error) |
//! | `io_buf` | Offset arithmetic for read/write (Kani-checked) |
//! | `xattr` | size=0 length protocol |
//! | `disk` | Inspect CAS after a live mount (test scaffold) |
//!
//! [`IMPLEMENTED_FUSE_OPS`] is the reachability contract. `tests/conformance.rs`
//! greps `fuse.rs` and this table. GitHub runs those tests **without** `/dev/fuse`.
//! Live kernel coverage is `tests/fuse_drive.rs` (skips when `CI` is set).
//!
//! Isolation: [`ArkSession::mount_store`] must not talk to peer nodes. See
//! `docs/maintainer.md`.

mod disk;
mod err;
mod fuse;
mod io_buf;
mod session;
mod xattr;

pub use disk::{inspect, DiskView, LiveNode};
pub use err::{all_errno_pairs, to_errno};
pub use fuse::{fuse_name, mount, spawn, FuseFs};
pub use io_buf::{apply_write, read_slice};
pub use session::{errno, fuse_kind, handle_to_attr, time_or_now_to_timespec, ArkSession, TTL};
pub use xattr::{encode_list, sized, SizedBytes};

/// FUSE low-level ops this facade implements (reachability contract).
///
/// Adding a `Filesystem` method requires: an entry here, a `fuse_*` test in
/// `tests/conformance.rs`, and (for kernel-visible ops) a step in `fuse_drive`.
pub const IMPLEMENTED_FUSE_OPS: &[&str] = &[
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
    "mount",
    "spawn",
];
