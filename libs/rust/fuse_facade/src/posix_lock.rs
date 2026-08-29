//! POSIX fcntl record locks (F_GETLK / F_SETLK / F_SETLKW).
//!
//! Byte ranges are inclusive on both ends (`end == u64::MAX` means “to EOF”),
//! matching the FUSE `fuse_file_lock` layout. Unlock splits a held range rather
//! than dropping the whole lock. `setlk(..., wait=true)` sleeps on a condvar;
//! the FUSE adapter must reply from another thread so the session loop can
//! still process the unlocking request.

use arkfs_core::ArkError;
use std::sync::{Condvar, Mutex};

#[derive(Clone, Debug)]
struct FileLock {
    ino: u64,
    owner: u64,
    start: u64,
    end: u64,
    typ: i32,
    pid: u32,
}

/// In-process POSIX record-lock table, keyed by inode + lock owner.
///
/// Maintainer: implemented in userspace. Advertised via FUSE_POSIX_LOCKS /
/// FLOCK_LOCKS in init. SETLKW must be replied from a helper thread.
/// See "POSIX locks" in maintainer.md and fuse.rs.
pub struct LockTable {
    locks: Mutex<Vec<FileLock>>,
    cv: Condvar,
}

impl Default for LockTable {
    /// Empty lock table.
    fn default() -> Self {
        LockTable {
            locks: Mutex::new(Vec::new()),
            cv: Condvar::new(),
        }
    }
}

impl LockTable {
    /// F_GETLK. Returns the blocker, or the request range with `F_UNLCK` if free.
    ///
    /// Maintainer: does not block. Used by FUSE getlk. See posix_lock module.
    #[allow(clippy::too_many_arguments)]
    pub fn getlk(
        &self,
        ino: u64,
        owner: u64,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
    ) -> (u64, u64, i32, u32) {
        let locks = self.locks.lock().unwrap();
        if let Some(c) = conflict(&locks, ino, owner, start, end, typ) {
            (c.start, c.end, c.typ, c.pid)
        } else {
            (start, end, libc::F_UNLCK, pid)
        }
    }

    /// F_SETLK / F_SETLKW. `wait` sleeps until the conflict is gone (or unlock).
    #[allow(clippy::too_many_arguments)]
    pub fn setlk(
        &self,
        ino: u64,
        owner: u64,
        start: u64,
        end: u64,
        typ: i32,
        pid: u32,
        wait: bool,
    ) -> Result<(), ArkError> {
        if typ == libc::F_UNLCK {
            let mut locks = self.locks.lock().unwrap();
            unlock(&mut locks, ino, owner, start, end);
            self.cv.notify_all();
            return Ok(());
        }
        loop {
            let mut locks = self.locks.lock().unwrap();
            if conflict(&locks, ino, owner, start, end, typ).is_some() {
                if !wait {
                    return Err(ArkError::busy("fcntl lock"));
                }
                let _g = self.cv.wait(locks).unwrap();
                continue;
            }
            unlock(&mut locks, ino, owner, start, end);
            locks.push(FileLock {
                ino,
                owner,
                start,
                end,
                typ,
                pid,
            });
            return Ok(());
        }
    }

    /// Drop every lock `owner` holds on `ino` (FUSE `flush` / last `close`).
    pub fn unlock_owner(&self, ino: u64, owner: u64) {
        let mut locks = self.locks.lock().unwrap();
        let n = locks.len();
        locks.retain(|l| l.ino != ino || l.owner != owner);
        if locks.len() != n {
            self.cv.notify_all();
        }
    }
}

/// Another owner holds a lock that conflicts with `typ` on this range.
fn conflict(
    locks: &[FileLock],
    ino: u64,
    owner: u64,
    start: u64,
    end: u64,
    typ: i32,
) -> Option<&FileLock> {
    locks.iter().find(|l| {
        l.ino == ino
            && l.owner != owner
            && ranges_overlap(l.start, l.end, start, end)
            && (l.typ == libc::F_WRLCK || typ == libc::F_WRLCK)
    })
}

/// Drop/split `owner`'s overlapping range on `ino`. Adjacent fragments are left as-is.
fn unlock(locks: &mut Vec<FileLock>, ino: u64, owner: u64, start: u64, end: u64) {
    let mut kept = Vec::with_capacity(locks.len());
    for l in locks.drain(..) {
        if l.ino != ino || l.owner != owner || !ranges_overlap(l.start, l.end, start, end) {
            kept.push(l);
            continue;
        }
        if l.start < start {
            kept.push(FileLock {
                end: start - 1,
                ..l.clone()
            });
        }
        if l.end > end {
            kept.push(FileLock {
                start: end.saturating_add(1),
                ..l
            });
        }
    }
    *locks = kept;
}

/// Inclusive ranges. Empty (`start > end`) never overlaps.
fn ranges_overlap(a0: u64, a1: u64, b0: u64, b1: u64) -> bool {
    a0 <= a1 && b0 <= b1 && a0 <= b1 && b0 <= a1
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use arkfs_test_review::{
        review_assert as assert, review_eq as assert_eq, review_ne as assert_ne,
    };
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    /// GETLK names the write-lock holder; after unlock it reports F_UNLCK.
    #[test]
    fn getlk_reports_blocker_then_unlck() {
        let _g = arkfs_test_review::guard();
        let t = LockTable::default();
        t.setlk(1, 10, 0, 9, libc::F_WRLCK, 42, false).unwrap();
        let (s, e, typ, pid) = t.getlk(1, 11, 0, 9, libc::F_RDLCK, 7);
        assert_eq!((s, e, typ, pid), (0, 9, libc::F_WRLCK, 42));
        t.setlk(1, 10, 0, 9, libc::F_UNLCK, 42, false).unwrap();
        let (_, _, typ, _) = t.getlk(1, 11, 0, 9, libc::F_WRLCK, 7);
        assert_eq!(typ, libc::F_UNLCK);
    }

    /// Unlocking the middle of a range leaves the two outer fragments.
    #[test]
    fn unlock_splits_range() {
        let _g = arkfs_test_review::guard();
        let t = LockTable::default();
        t.setlk(1, 1, 0, 99, libc::F_WRLCK, 1, false).unwrap();
        t.setlk(1, 1, 40, 49, libc::F_UNLCK, 1, false).unwrap();
        let (_, _, typ, _) = t.getlk(1, 2, 0, 9, libc::F_WRLCK, 2);
        assert_eq!(typ, libc::F_WRLCK);
        let (_, _, typ, _) = t.getlk(1, 2, 40, 49, libc::F_WRLCK, 2);
        assert_eq!(typ, libc::F_UNLCK);
        let (_, _, typ, _) = t.getlk(1, 2, 50, 99, libc::F_WRLCK, 2);
        assert_eq!(typ, libc::F_WRLCK);
    }

    /// SETLKW blocks until the other owner unlocks.
    #[test]
    fn setlkw_waits_then_unblocks() {
        let _g = arkfs_test_review::guard();
        let t = Arc::new(LockTable::default());
        t.setlk(1, 10, 0, 10, libc::F_WRLCK, 1, false).unwrap();
        let waiter = t.clone();
        let th = thread::spawn(move || {
            waiter.setlk(1, 20, 0, 10, libc::F_WRLCK, 2, true).unwrap();
        });
        thread::sleep(Duration::from_millis(40));
        t.setlk(1, 10, 0, 10, libc::F_UNLCK, 1, false).unwrap();
        th.join().unwrap();
    }

    /// SETLK without wait returns Busy (EAGAIN) on conflict.
    #[test]
    fn nonblocking_conflict_is_busy() {
        let _g = arkfs_test_review::guard();
        let t = LockTable::default();
        t.setlk(1, 1, 0, 1, libc::F_WRLCK, 1, false).unwrap();
        let err = t.setlk(1, 2, 0, 1, libc::F_WRLCK, 2, false).unwrap_err();
        assert!(matches!(err, ArkError::Busy { .. }));
    }

    /// Adjacent ranges and empty (start > end) do not overlap; two readers share.
    #[test]
    fn adjacent_empty_and_shared_read_locks() {
        let _g = arkfs_test_review::guard();
        let t = LockTable::default();
        t.setlk(1, 1, 0, 9, libc::F_RDLCK, 1, false).unwrap();
        t.setlk(1, 2, 10, 19, libc::F_RDLCK, 2, false).unwrap();
        let (_, _, typ, _) = t.getlk(1, 3, 0, 9, libc::F_RDLCK, 3);
        assert_eq!(typ, libc::F_UNLCK);
        t.setlk(1, 3, 0, 9, libc::F_RDLCK, 3, false).unwrap();
        let err = t.setlk(1, 4, 0, 9, libc::F_WRLCK, 4, false).unwrap_err();
        assert!(matches!(err, ArkError::Busy { .. }));
        t.setlk(2, 1, 5, 4, libc::F_WRLCK, 1, false).unwrap();
        let (_, _, typ, _) = t.getlk(2, 2, 0, 10, libc::F_WRLCK, 2);
        assert_eq!(typ, libc::F_UNLCK);
    }

    /// Same owner may overlap (upgrade); unlock_owner drops every range on that ino.
    #[test]
    fn same_owner_upgrade_and_unlock_owner() {
        let _g = arkfs_test_review::guard();
        let t = LockTable::default();
        t.setlk(1, 1, 0, 9, libc::F_RDLCK, 1, false).unwrap();
        t.setlk(1, 1, 0, 9, libc::F_WRLCK, 1, false).unwrap();
        t.setlk(1, 1, 20, 29, libc::F_WRLCK, 1, false).unwrap();
        t.unlock_owner(1, 1);
        let (_, _, typ, _) = t.getlk(1, 2, 0, 29, libc::F_WRLCK, 2);
        assert_eq!(typ, libc::F_UNLCK);
    }
}
