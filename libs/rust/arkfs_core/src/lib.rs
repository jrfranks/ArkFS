//! ArkFS shared types and the canonical file-attribute superset.
//!
//! This crate is a pure library: no I/O. Protocol facades project to/from
//! [`FileAttributes`] via [`attr_map`] without using any protocol as source of truth.

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
