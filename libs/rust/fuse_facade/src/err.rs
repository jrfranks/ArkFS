//! [`ArkError`] → FUSE errno. Total match: a new error variant fails to compile.
//!
//! Integrity and quorum failures become `EIO` (the kernel cannot distinguish
//! “bit flip” from “disk died”). `Conflict` becomes `EPERM`.
//!
//! Maintainer: this must be a total match over ArkError. Adding a variant
//! without a case here is a compile error. See "to_errno" and ArkError docs.

use arkfs_core::ArkError;
use libc::{
    EACCES, EAGAIN, EEXIST, EFBIG, EINVAL, EIO, EISDIR, ENAMETOOLONG, ENOENT, ENOSPC, ENOSYS,
    ENOTDIR, ENOTEMPTY, ENXIO, EPERM, EROFS,
};

/// Map a library error to the errno the kernel expects on the FUSE reply.
///
/// Maintainer: keep in sync with all_errno_pairs() test. Integrity/Quorum → EIO.
/// Conflict → EPERM (not a direct POSIX equivalent for our use).
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
        ArkError::Busy { .. } => EAGAIN,
        ArkError::PermissionDenied { .. } => EACCES,
        ArkError::NoSuchDevice { .. } => ENXIO,
        ArkError::NameTooLong { .. } => ENAMETOOLONG,
        ArkError::NoSpace { .. } => ENOSPC,
        ArkError::FileTooLarge { .. } => EFBIG,
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
        (ArkError::busy("x"), EAGAIN),
        (ArkError::permission_denied("x"), EACCES),
        (ArkError::no_such_device("x"), ENXIO),
        (ArkError::name_too_long("x"), ENAMETOOLONG),
        (ArkError::no_space("x"), ENOSPC),
        (ArkError::file_too_large("x"), EFBIG),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use arkfs_test_review::{
        review_assert as assert, review_eq as assert_eq, review_ne as assert_ne,
    };

    /// Spot-check ENOENT / EEXIST / EROFS mapping.
    #[test]
    fn maps_posix_errors() {
        let _g = arkfs_test_review::guard();
        assert_eq!(to_errno(&ArkError::not_found("/x")), ENOENT);
        assert_eq!(to_errno(&ArkError::already_exists("/x")), EEXIST);
        assert_eq!(to_errno(&ArkError::ReadOnly), EROFS);
        assert_eq!(to_errno(&ArkError::permission_denied("x")), EACCES);
        assert_eq!(to_errno(&ArkError::no_such_device("x")), ENXIO);
        assert_ne!(to_errno(&ArkError::no_such_device("x")), EINVAL);
        assert_ne!(to_errno(&ArkError::permission_denied("x")), EINVAL);
        assert_eq!(to_errno(&ArkError::name_too_long("x")), ENAMETOOLONG);
        assert_eq!(to_errno(&ArkError::no_space("x")), ENOSPC);
        assert_eq!(to_errno(&ArkError::file_too_large("x")), EFBIG);
    }

    /// Every ArkError variant in all_errno_pairs maps to the listed errno.
    #[test]
    fn errno_table_covers_every_variant() {
        let _g = arkfs_test_review::guard();
        for (err, want) in all_errno_pairs() {
            assert_eq!(to_errno(&err), want, "{err}");
        }
    }
}
