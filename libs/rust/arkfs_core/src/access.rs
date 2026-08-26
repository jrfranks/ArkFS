//! Unix permission bits used by FUSE `access` / `open` / directory mutate.
//!
//! Mask values match POSIX `F_OK` / `X_OK` / `W_OK` / `R_OK` (`unistd.h`).
//! Root is `caller_uid == 0` only (gid 0 is not special). Group class is the
//! file gid vs the caller primary gid or any extra gid.

use crate::attributes::{AceType, AclEntry, FileAttributes, Principal};

/// POSIX `F_OK` — existence only (always allowed once the object was looked up).
pub const ACCESS_F: u32 = 0;
/// POSIX `X_OK`.
pub const ACCESS_X: u32 = 1;
/// POSIX `W_OK`.
pub const ACCESS_W: u32 = 2;
/// POSIX `R_OK`.
pub const ACCESS_R: u32 = 4;

/// Directory sticky bit (`S_ISVTX`). Unlink/rename of someone else's name is denied.
pub const MODE_STICKY: u32 = 0o1000;
/// Set-group-ID (`S_ISGID`). New children inherit the directory gid.
pub const MODE_SETGID: u32 = 0o2000;

/// True when `caller_uid`/`caller_gid` may use `mask` on a file with these bits.
///
/// - `mask` with no R/W/X bits (`F_OK`) is always true.
/// - Root may read and write always. Execute is allowed only when some
///   execute bit is set (`mode & 0o111`).
/// - Otherwise owner / group / other class is chosen, then the requested
///   bits must all be present in that class.
pub fn unix_access(
    mode: u32,
    file_uid: u32,
    file_gid: u32,
    caller_uid: u32,
    caller_gid: u32,
    mask: u32,
) -> bool {
    unix_access_gids(mode, file_uid, file_gid, caller_uid, caller_gid, &[], mask)
}

/// [`unix_access`] with supplementary group ids (FUSE may pass only the primary).
pub fn unix_access_gids(
    mode: u32,
    file_uid: u32,
    file_gid: u32,
    caller_uid: u32,
    caller_gid: u32,
    extra_gids: &[u32],
    mask: u32,
) -> bool {
    let need = mask & (ACCESS_R | ACCESS_W | ACCESS_X);
    if need == 0 {
        return true;
    }
    if caller_uid == 0 {
        return need & ACCESS_X == 0 || mode & 0o111 != 0;
    }
    let in_group = caller_gid == file_gid || extra_gids.contains(&file_gid);
    let shift = if caller_uid == file_uid {
        6
    } else if in_group {
        3
    } else {
        0
    };
    let granted = (mode >> shift) & 0o7;
    let mut want = 0u32;
    if need & ACCESS_R != 0 {
        want |= 4;
    }
    if need & ACCESS_W != 0 {
        want |= 2;
    }
    if need & ACCESS_X != 0 {
        want |= 1;
    }
    granted & want == want
}

/// True when a sticky parent allows `caller_uid` to unlink/rename `file_uid`.
///
/// Non-sticky parents always allow this check (caller still needs W+X on the
/// directory). Sticky: root, directory owner, or file owner.
pub fn sticky_allows_unlink(
    parent_mode: u32,
    parent_uid: u32,
    file_uid: u32,
    caller_uid: u32,
) -> bool {
    if parent_mode & MODE_STICKY == 0 {
        return true;
    }
    caller_uid == 0 || caller_uid == parent_uid || caller_uid == file_uid
}

/// Apply `umask` to permission bits (type bits in `S_IFMT` are left to the caller).
pub fn apply_umask(mode: u32, umask: u32) -> u32 {
    mode & !umask & 0o7777
}

/// New file/dir gid and mode under a setgid parent.
///
/// Setgid directory: child gid = parent gid. A new directory also copies the
/// setgid bit so the property continues down the tree.
pub fn inherit_from_parent(
    parent_mode: u32,
    parent_gid: u32,
    child_gid: u32,
    child_mode: u32,
    child_is_dir: bool,
) -> (u32, u32) {
    if parent_mode & MODE_SETGID == 0 {
        return (child_gid, child_mode);
    }
    let mut mode = child_mode;
    if child_is_dir {
        mode |= MODE_SETGID;
    }
    (parent_gid, mode)
}

/// Permission check using Unix bits, then stored ACLs if the list is non-empty.
///
/// Deny ACEs that match the caller and cover the requested bits win. Then
/// Allow ACEs. If no ACE decides, Unix mode is used.
pub fn allows(attrs: &FileAttributes, caller_uid: u32, caller_gid: u32, mask: u32) -> bool {
    allows_gids(attrs, caller_uid, caller_gid, &[], mask)
}

/// [`allows`] with supplementary groups.
pub fn allows_gids(
    attrs: &FileAttributes,
    caller_uid: u32,
    caller_gid: u32,
    extra_gids: &[u32],
    mask: u32,
) -> bool {
    let need = mask & (ACCESS_R | ACCESS_W | ACCESS_X);
    if need == 0 {
        return true;
    }
    if attrs.acl.is_empty() {
        return unix_access_gids(
            attrs.mode, attrs.uid, attrs.gid, caller_uid, caller_gid, extra_gids, mask,
        );
    }
    for ace in &attrs.acl {
        if ace.flags.inherit_only {
            continue;
        }
        if !principal_matches(ace, attrs, caller_uid, caller_gid, extra_gids) {
            continue;
        }
        if ace.ace_type == AceType::Deny && ace_covers(&ace.access, need) {
            return false;
        }
    }
    for ace in &attrs.acl {
        if ace.flags.inherit_only {
            continue;
        }
        if ace.ace_type != AceType::Allow {
            continue;
        }
        if principal_matches(ace, attrs, caller_uid, caller_gid, extra_gids)
            && ace_covers(&ace.access, need)
        {
            return true;
        }
    }
    unix_access_gids(
        attrs.mode, attrs.uid, attrs.gid, caller_uid, caller_gid, extra_gids, mask,
    )
}

fn principal_matches(
    ace: &AclEntry,
    attrs: &FileAttributes,
    caller_uid: u32,
    caller_gid: u32,
    extra_gids: &[u32],
) -> bool {
    match &ace.principal {
        Principal::Everyone | Principal::Authenticated => true,
        Principal::Owner => caller_uid == attrs.uid,
        Principal::Group => caller_gid == attrs.gid || extra_gids.contains(&attrs.gid),
        Principal::Unix { uid, gid } => {
            caller_uid == *uid || gid.is_some_and(|g| g == caller_gid || extra_gids.contains(&g))
        }
        Principal::Name(_) | Principal::Sid(_) => false,
    }
}

fn ace_covers(a: &crate::attributes::AceAccess, need: u32) -> bool {
    if need & ACCESS_R != 0 && !a.read_data {
        return false;
    }
    if need & ACCESS_W != 0 && !(a.write_data || a.append_data) {
        return false;
    }
    if need & ACCESS_X != 0 && !a.execute {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attributes::AceAccess;
    #[allow(unused_imports)]
    use arkfs_test_review::{
        review_assert as assert, review_eq as assert_eq, review_ne as assert_ne,
    };

    /// F_OK (mask 0) is existence only and does not inspect bits.
    #[test]
    fn f_ok_always_true() {
        let _g = arkfs_test_review::guard();
        assert!(unix_access(0, 1, 1, 99, 99, ACCESS_F));
        assert!(unix_access(0, 1, 1, 99, 99, 0));
    }

    /// Owner class uses bits 8-6; group/other bits are ignored for the owner.
    #[test]
    fn owner_class_not_other() {
        let _g = arkfs_test_review::guard();
        assert!(unix_access(
            0o700,
            5,
            5,
            5,
            5,
            ACCESS_R | ACCESS_W | ACCESS_X
        ));
        assert!(!unix_access(0o007, 5, 5, 5, 9, ACCESS_R));
        assert!(unix_access(0o400, 5, 1, 5, 1, ACCESS_R));
        assert!(!unix_access(0o400, 5, 1, 5, 1, ACCESS_W));
    }

    /// Group class when uids differ and gids match.
    #[test]
    fn group_class() {
        let _g = arkfs_test_review::guard();
        assert!(unix_access(0o040, 1, 7, 2, 7, ACCESS_R));
        assert!(!unix_access(0o040, 1, 7, 2, 7, ACCESS_W));
        assert!(!unix_access(0o040, 1, 7, 2, 8, ACCESS_R));
        assert!(unix_access(0o004, 1, 7, 2, 8, ACCESS_R));
    }

    /// Other class is last resort.
    #[test]
    fn other_class() {
        let _g = arkfs_test_review::guard();
        assert!(unix_access(0o644, 0, 0, 9, 9, ACCESS_R));
        assert!(!unix_access(0o644, 0, 0, 9, 9, ACCESS_W));
        assert!(!unix_access(0o644, 0, 0, 9, 9, ACCESS_X));
        assert!(unix_access(0o755, 0, 0, 9, 9, ACCESS_R | ACCESS_X));
        assert!(!unix_access(0o755, 0, 0, 9, 9, ACCESS_W));
    }

    /// Root bypasses read/write; execute still needs some x bit.
    #[test]
    fn root_bypass() {
        let _g = arkfs_test_review::guard();
        assert!(unix_access(0, 9, 9, 0, 1, ACCESS_R | ACCESS_W));
        assert!(!unix_access(0, 9, 9, 0, 1, ACCESS_X));
        assert!(unix_access(0o001, 9, 9, 0, 1, ACCESS_X));
        assert!(unix_access(0o010, 9, 9, 0, 1, ACCESS_X));
        assert!(unix_access(0o100, 9, 9, 0, 1, ACCESS_X));
        assert!(unix_access(0, 9, 9, 0, 0, ACCESS_R));
    }

    /// gid 0 is not root; extra mask bits are ignored.
    #[test]
    fn gid_zero_is_not_root_and_extra_bits_ignored() {
        let _g = arkfs_test_review::guard();
        assert!(!unix_access(0o000, 1, 0, 2, 0, ACCESS_R));
        assert!(unix_access(0o400, 1, 1, 1, 1, ACCESS_R | 0o70));
    }

    /// Directory search (X) for other vs file execute.
    #[test]
    fn execute_directory_vs_file() {
        let _g = arkfs_test_review::guard();
        assert!(unix_access(0o755, 0, 0, 9, 9, ACCESS_X));
        assert!(!unix_access(0o644, 0, 0, 9, 9, ACCESS_X));
        assert!(!unix_access(0o666, 0, 0, 9, 9, ACCESS_X));
        assert!(unix_access(0o711, 1, 1, 1, 1, ACCESS_X));
    }

    /// Combined R+W must have both bits in the chosen class.
    #[test]
    fn combined_mask() {
        let _g = arkfs_test_review::guard();
        assert!(unix_access(0o600, 3, 3, 3, 3, ACCESS_R | ACCESS_W));
        assert!(!unix_access(0o400, 3, 3, 3, 3, ACCESS_R | ACCESS_W));
        assert!(!unix_access(0o200, 3, 3, 3, 3, ACCESS_R | ACCESS_W));
    }

    /// Sticky: off → allow; on → root, dir owner, or file owner.
    #[test]
    fn sticky_unlink_rules() {
        let _g = arkfs_test_review::guard();
        assert!(sticky_allows_unlink(0o777, 1, 2, 99));
        assert!(sticky_allows_unlink(0o1777, 1, 2, 0));
        assert!(sticky_allows_unlink(0o1777, 1, 2, 1));
        assert!(sticky_allows_unlink(0o1777, 1, 2, 2));
        assert!(!sticky_allows_unlink(0o1777, 1, 2, 99));
    }

    /// umask clears permission bits only.
    #[test]
    fn umask_clears_perm_bits() {
        let _g = arkfs_test_review::guard();
        assert_eq!(apply_umask(0o777, 0o022), 0o755);
        assert_eq!(apply_umask(0o666, 0o022), 0o644);
        assert_eq!(apply_umask(0o777, 0), 0o777);
    }

    /// setgid parent: child gid inherited; directories copy the bit.
    #[test]
    fn setgid_inherit() {
        let _g = arkfs_test_review::guard();
        assert_eq!(inherit_from_parent(0o755, 1, 9, 0o644, false), (9, 0o644));
        assert_eq!(inherit_from_parent(0o2755, 1, 9, 0o644, false), (1, 0o644));
        assert_eq!(inherit_from_parent(0o2755, 1, 9, 0o755, true), (1, 0o2755));
    }

    /// Supplementary gid grants group-class bits.
    #[test]
    fn extra_gid_is_group_class() {
        let _g = arkfs_test_review::guard();
        assert!(!unix_access(0o040, 1, 7, 2, 8, ACCESS_R));
        assert!(unix_access_gids(0o040, 1, 7, 2, 8, &[7], ACCESS_R));
    }

    /// ACL Deny Everyone write blocks even when mode is 0666.
    #[test]
    fn acl_deny_everyone_write() {
        let _g = arkfs_test_review::guard();
        let mut a = FileAttributes::new_file(1, 0o666);
        a.acl.push(AclEntry {
            principal: Principal::Everyone,
            ace_type: AceType::Deny,
            access: AceAccess {
                write_data: true,
                ..Default::default()
            },
            flags: Default::default(),
        });
        assert!(allows(&a, 9, 9, ACCESS_R));
        assert!(!allows(&a, 9, 9, ACCESS_W));
    }

    /// ACL Allow Everyone execute grants X when mode has none.
    #[test]
    fn acl_allow_everyone_execute() {
        let _g = arkfs_test_review::guard();
        let mut a = FileAttributes::new_file(1, 0o644);
        a.acl.push(AclEntry {
            principal: Principal::Everyone,
            ace_type: AceType::Allow,
            access: AceAccess {
                execute: true,
                ..Default::default()
            },
            flags: Default::default(),
        });
        assert!(allows(&a, 9, 9, ACCESS_X));
    }
}
