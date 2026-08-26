//! [`ArkError`] → FUSE errno. Total match: a new error variant fails to compile.
//!
//! Integrity and quorum failures become `EIO` (the kernel cannot distinguish
//! “bit flip” from “disk died”). `Conflict` becomes `EPERM`.

use arkfs_core::ArkError;
use libc::{EEXIST, EINVAL, EIO, EISDIR, ENOENT, ENOSYS, ENOTDIR, ENOTEMPTY, EPERM, EROFS};

/// Map a library error to the errno the kernel expects on the FUSE reply.
pub fn to_errno(err: &ArkError) -> i32 {
    match err {
        ArkError::NotFound { .. } => ENOENT,
        ArkError::AlreadyExists { .. } => EEXIST,
        ArkError::IsADirectory { .. } => EISDIR,
        ArkError::NotADirectory { .. } => ENOTDIR,
        ArkError::NotEmpty { .. } => ENOTEMPTY,
        ArkError::ReadOnly => EROFS,
        ArkError::InvalidArgument { .. } | ArkError::AttrInvalid { .. } => EINVAL,
        ArkError::Conflict { .. } => EPERM,
        ArkError::NotImplemented { .. } => ENOSYS,
        ArkError::Io {
            kind: std::io::ErrorKind::NotFound,
            ..
        } => ENOENT,
        ArkError::Integrity { .. } | ArkError::Quorum { .. } | ArkError::Io { .. } => EIO,
    }
}

/// Every `ArkError` arm is named so adding a variant without an errno is a compile error.
pub fn all_errno_pairs() -> Vec<(ArkError, i32)> {
    use std::io::ErrorKind;
    vec![
        (ArkError::not_found("x"), ENOENT),
        (ArkError::integrity("x"), EIO),
        (ArkError::Quorum { got: 0, need: 1 }, EIO),
        (
            ArkError::Io {
                kind: ErrorKind::NotFound,
                message: "x".into(),
            },
            ENOENT,
        ),
        (
            ArkError::Io {
                kind: ErrorKind::Other,
                message: "x".into(),
            },
            EIO,
        ),
        (ArkError::attr_invalid("x"), EINVAL),
        (ArkError::invalid_argument("x"), EINVAL),
        (ArkError::conflict("x"), EPERM),
        (ArkError::already_exists("x"), EEXIST),
        (ArkError::is_a_directory("x"), EISDIR),
        (ArkError::not_a_directory("x"), ENOTDIR),
        (ArkError::not_empty("x"), ENOTEMPTY),
        (ArkError::ReadOnly, EROFS),
        (ArkError::not_implemented("x"), ENOSYS),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_posix_errors() {
        assert_eq!(to_errno(&ArkError::not_found("/x")), ENOENT);
        assert_eq!(to_errno(&ArkError::already_exists("/x")), EEXIST);
        assert_eq!(to_errno(&ArkError::ReadOnly), EROFS);
    }

    #[test]
    fn errno_table_covers_every_variant() {
        for (err, want) in all_errno_pairs() {
            assert_eq!(to_errno(&err), want, "{err}");
        }
    }
}
