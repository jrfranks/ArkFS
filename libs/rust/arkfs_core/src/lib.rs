//! ArkFS shared types and the canonical file-attribute superset.
//!
//! Pure library: no I/O, no threads, no FUSE. Every other crate depends on this
//! one. If a concept exists in both TemporalCore and FUSE (path, error, inode
//! id, timestamp), it belongs here.
//!
//! # Modules a maintainer actually edits
//!
//! - [`id`]: [`ObjectId`] (BLAKE3 of bytes), [`PathKey`] (absolute POSIX path
//!   with no `.` / `..` / NUL), [`OwnerId`].
//! - [`error`]: [`ArkError`] — map new variants in `fuse_facade::err::to_errno`
//!   or GitHub CI will still compile but FUSE will not translate the error.
//! - [`attributes`]: [`FileAttributes`] is source of truth. Facades **project**.
//! - [`codec`]: lossless binary for attrs (`ARKA1` + BLAKE3 trailer).
//! - [`attr_map`]: `to_*` / `merge_from_*` so a FUSE setattr cannot wipe SMB flags.
//! - [`quorum`]: [`QuorumPolicy`] math used by the object store.
//! - [`time`]: hybrid logical + wall [`Timestamp`].
//!
//! Onboarding: `docs/maintainer.md`.

pub mod attr_map;
pub mod attributes;
pub mod codec;
pub mod error;
pub mod id;
pub mod quorum;
pub mod time;

pub use attributes::{
    AceAccess, AceFlags, AceType, AclEntry, DosFlags, FileAttributes, FileType, MacOsFlags,
    NamedStream, PosixPatch, Principal, SizePolicy, Timespec,
};
pub use error::ArkError;
pub use id::{ObjectId, OwnerId, PathKey};
pub use quorum::QuorumPolicy;
pub use time::Timestamp;
