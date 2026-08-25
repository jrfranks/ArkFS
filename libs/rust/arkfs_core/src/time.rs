use crate::attributes::Timespec;
use serde::{Deserialize, Serialize};

/// Hybrid timestamp: logical counter for distributed order + wall-clock for protocols/UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Timestamp {
    /// Monotonic logical component (Lamport-style / HLC logical).
    pub logical: u64,
    /// Wall-clock nanoseconds since Unix epoch (best-effort).
    pub wall_nanos: u64,
}

impl Timestamp {
    pub const ZERO: Timestamp = Timestamp {
        logical: 0,
        wall_nanos: 0,
    };

    pub fn new(logical: u64, wall_nanos: u64) -> Self {
        Timestamp {
            logical,
            wall_nanos,
        }
    }

    pub fn tick(self, wall_nanos: u64) -> Self {
        Timestamp {
            logical: self.logical.saturating_add(1),
            wall_nanos: wall_nanos.max(self.wall_nanos),
        }
    }

    pub fn merge(self, other: Timestamp) -> Timestamp {
        Timestamp {
            logical: self.logical.max(other.logical).saturating_add(1),
            wall_nanos: self.wall_nanos.max(other.wall_nanos),
        }
    }

    pub fn to_timespec(self) -> Timespec {
        Timespec::from_nanos(self.wall_nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_advances_logical() {
        let t0 = Timestamp::new(1, 100);
        let t1 = t0.tick(200);
        assert_eq!(t1.logical, 2);
        assert_eq!(t1.wall_nanos, 200);
    }

    #[test]
    fn merge_takes_max_and_ticks() {
        let a = Timestamp::new(5, 10);
        let b = Timestamp::new(3, 50);
        let m = a.merge(b);
        assert_eq!(m.logical, 6);
        assert_eq!(m.wall_nanos, 50);
    }

    #[test]
    fn timespec_bridge() {
        let ts = Timestamp::new(1, 1_500_000_000);
        let wall = ts.to_timespec();
        assert_eq!(wall.sec, 1);
        assert_eq!(wall.nsec, 500_000_000);
    }
}
