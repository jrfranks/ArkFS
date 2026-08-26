//! Shared [`ArkError`] for every ArkFS crate.
//!
//! Keep this enum POSIX-shaped enough that `fuse_facade::err::to_errno` can be a
//! total `match`. Adding a variant **without** updating that match is a compile
//! error (intentional). `Conflict` is not a POSIX errno; FUSE maps it to `EPERM`.
//!
//! Constructors take `impl Into<String>` so call sites can pass `&str` or
//! `String` without `.to_string()` noise. Prefer constructors over building
//! variants by hand so display text stays consistent.

use std::io;
use thiserror::Error;

/// Recoverable failure shared by store, temporal, and FUSE layers.
///
/// `Io` stores `ErrorKind` + message instead of `std::io::Error` so `ArkError`
/// can be `Clone + Eq` (needed by tests and Kani).
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ArkError {
    #[error("not found: {what}")]
    NotFound { what: String },

    #[error("integrity check failed: {detail}")]
    Integrity { detail: String },

    #[error("quorum not satisfied: got {got} acks, need {need}")]
    Quorum { got: u32, need: u32 },

    #[error("I/O error ({kind:?}): {message}")]
    Io {
        kind: io::ErrorKind,
        message: String,
    },

    #[error("invalid attribute: {detail}")]
    AttrInvalid { detail: String },

    #[error("invalid argument: {detail}")]
    InvalidArgument { detail: String },

    #[error("conflict: {detail}")]
    Conflict { detail: String },

    #[error("already exists: {what}")]
    AlreadyExists { what: String },

    #[error("is a directory: {what}")]
    IsADirectory { what: String },

    #[error("not a directory: {what}")]
    NotADirectory { what: String },

    #[error("directory not empty: {what}")]
    NotEmpty { what: String },

    #[error("read-only file system")]
    ReadOnly,

    #[error("not implemented: {detail}")]
    NotImplemented { detail: String },

    #[error("resource busy: {detail}")]
    Busy { detail: String },

    #[error("permission denied: {what}")]
    PermissionDenied { what: String },

    #[error("no such device: {what}")]
    NoSuchDevice { what: String },

    #[error("name too long: {what}")]
    NameTooLong { what: String },

    #[error("no space left on device: {detail}")]
    NoSpace { detail: String },

    #[error("file too large: {detail}")]
    FileTooLarge { detail: String },
}

impl ArkError {
    /// Name or object id that lookup failed for.
    pub fn not_found(what: impl Into<String>) -> Self {
        ArkError::NotFound { what: what.into() }
    }

    /// Checksum mismatch, bad magic, truncated record — fail closed.
    pub fn integrity(detail: impl Into<String>) -> Self {
        ArkError::Integrity {
            detail: detail.into(),
        }
    }

    /// Bad path component, negative seek, malformed CLI, etc.
    pub fn invalid_argument(detail: impl Into<String>) -> Self {
        ArkError::InvalidArgument {
            detail: detail.into(),
        }
    }

    /// Commit timestamp not strictly after the path's latest version.
    pub fn conflict(detail: impl Into<String>) -> Self {
        ArkError::Conflict {
            detail: detail.into(),
        }
    }

    /// Create/mkdir on a live name (tombstones do not trigger this).
    pub fn already_exists(what: impl Into<String>) -> Self {
        ArkError::AlreadyExists { what: what.into() }
    }

    /// unlink / replace_content on a directory.
    pub fn is_a_directory(what: impl Into<String>) -> Self {
        ArkError::IsADirectory { what: what.into() }
    }

    /// readdir / mkdir-child on a non-directory.
    pub fn not_a_directory(what: impl Into<String>) -> Self {
        ArkError::NotADirectory { what: what.into() }
    }

    /// rmdir of a directory that still has live children.
    pub fn not_empty(what: impl Into<String>) -> Self {
        ArkError::NotEmpty { what: what.into() }
    }

    /// Attribute field failed validation (reserved for facades).
    pub fn attr_invalid(detail: impl Into<String>) -> Self {
        ArkError::AttrInvalid {
            detail: detail.into(),
        }
    }

    /// Feature the facade refuses (unsupported fallocate mode).
    pub fn not_implemented(detail: impl Into<String>) -> Self {
        ArkError::NotImplemented {
            detail: detail.into(),
        }
    }

    /// POSIX `EAGAIN` / `EWOULDBLOCK` (fcntl lock conflict).
    pub fn busy(detail: impl Into<String>) -> Self {
        ArkError::Busy {
            detail: detail.into(),
        }
    }

    /// POSIX `EACCES` (Unix permission bits, sticky bit, chmod/chown).
    pub fn permission_denied(what: impl Into<String>) -> Self {
        ArkError::PermissionDenied { what: what.into() }
    }

    /// POSIX `ENXIO` (open of fifo/device/socket with no userspace driver).
    pub fn no_such_device(what: impl Into<String>) -> Self {
        ArkError::NoSuchDevice { what: what.into() }
    }

    /// POSIX `ENAMETOOLONG` (path component longer than 255 bytes).
    pub fn name_too_long(what: impl Into<String>) -> Self {
        ArkError::NameTooLong { what: what.into() }
    }

    /// POSIX `ENOSPC` (backing disk full during a durable write).
    pub fn no_space(detail: impl Into<String>) -> Self {
        ArkError::NoSpace {
            detail: detail.into(),
        }
    }

    /// POSIX `EFBIG` (write would exceed the in-memory object size cap).
    pub fn file_too_large(detail: impl Into<String>) -> Self {
        ArkError::FileTooLarge {
            detail: detail.into(),
        }
    }
}

impl From<io::Error> for ArkError {
    /// Disk-full becomes [`ArkError::NoSpace`]; other I/O stays `Io`.
    fn from(e: io::Error) -> Self {
        // ENOSPC is 28 on Linux/macOS; ERROR_DISK_FULL is 112 on Windows.
        match e.raw_os_error() {
            Some(28) | Some(112) => return ArkError::no_space(e.to_string()),
            _ => {}
        }
        ArkError::Io {
            kind: e.kind(),
            message: e.to_string(),
        }
    }
}
