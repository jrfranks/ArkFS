//! Hybrid logical clock used as the temporal index's version time.
//!
//! Ordering is `(logical, wall_nanos)` via derive `Ord`. `--as-of N` on the CLI
//! is **logical** only (`Timestamp::new(N, u64::MAX)` so the whole logical
//! tick is visible). Never use wall-clock alone for history cuts: two nodes
//! (and even one fast machine) can share a wall second.

use crate::attributes::Timespec;
use serde::{Deserialize, Serialize};

/// Hybrid timestamp: logical counter for distributed order + wall-clock for protocols/UI.
///
/// Maintainer: stored per VersionRecord. Ord is (logical, wall). Used for
/// Live/AsOf and commit conflict checks. Never trust wall alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Timestamp {
    /// Monotonic logical component (Lamport-style / HLC logical).
    pub logical: u64,
    /// Wall-clock nanoseconds since Unix epoch (best-effort).
    pub wall_nanos: u64,
}

impl Timestamp {
    /// The zero timestamp. Used as initial max seed for clock.
    pub const ZERO: Timestamp = Timestamp {
        logical: 0,
        wall_nanos: 0,
    };

    /// Timestamp from logical tick + wall nanos.
    ///
    /// Maintainer: prefer tick() / merge() / observe() for advancing.
    /// Direct use is mostly for --as-of and tests.
    pub fn new(logical: u64, wall_nanos: u64) -> Self {
        Timestamp {
            logical,
            wall_nanos,
        }
    }

    /// Next event on this node: `logical + 1`, wall = max(now, previous wall).
    ///
    /// Maintainer: guarantees per-node monotonic logical. See TemporalCore::tick.
    pub fn tick(self, wall_nanos: u64) -> Self {
        Timestamp {
            logical: self.logical.saturating_add(1),
            wall_nanos: wall_nanos.max(self.wall_nanos),
        }
    }

    /// Happens-after both `self` and `other` (max logical + 1, max wall).
    ///
    /// Maintainer: used when incorporating observed remote timestamps.
    pub fn merge(self, other: Timestamp) -> Timestamp {
        Timestamp {
            logical: self.logical.max(other.logical).saturating_add(1),
            wall_nanos: self.wall_nanos.max(other.wall_nanos),
        }
    }

    /// Protocol-facing wall time (drops the logical component).
    ///
    /// Maintainer: for atime/mtime/ctime projection to FUSE etc. Loses ordering
    /// info. See "to_timespec".
    pub fn to_timespec(self) -> Timespec {
        Timespec::from_nanos(self.wall_nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use arkfs_test_review::{
        review_assert as assert, review_eq as assert_eq, review_ne as assert_ne,
    };

    /// tick always increments logical.
    #[test]
    fn tick_advances_logical() {
        let _g = arkfs_test_review::guard();
        let t0 = Timestamp::new(1, 100);
        let t1 = t0.tick(200);
        assert_eq!(t1.logical, 2);
        assert_eq!(t1.wall_nanos, 200);
    }

    /// merge takes componentwise max then ticks.
    #[test]
    fn merge_takes_max_and_ticks() {
        let _g = arkfs_test_review::guard();
        let a = Timestamp::new(5, 10);
        let b = Timestamp::new(3, 50);
        let m = a.merge(b);
        assert_eq!(m.logical, 6);
        assert_eq!(m.wall_nanos, 50);
    }

    /// Timestamp ↔ Timespec via wall_nanos.
    #[test]
    fn timespec_bridge() {
        let _g = arkfs_test_review::guard();
        let ts = Timestamp::new(1, 1_500_000_000);
        let wall = ts.to_timespec();
        assert_eq!(wall.sec, 1);
        assert_eq!(wall.nsec, 500_000_000);
    }
}
