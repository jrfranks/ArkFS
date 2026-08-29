//! How many durable copies count as “safe” for [`persistent_object_store`].
//!
//! The store always counts the **local** write as one ack. `always_on` passed
//! into these methods is reachable copies **including local**. Isolated
//! single-node (zero remotes) therefore has `always_on == 1`.
//!
//! `OwnerOnly` is the FUSE default. [`QuorumPolicy::n`] requires `n >= 1`
//! (`n == 0` panics). `n > 1` with no remotes must fail closed — do not
//! lower `n` silently.

use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

/// Replication / durability policy for safe-write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuorumPolicy {
    /// Require acknowledgment from `n` copies (local durable write counts as one).
    ///
    /// `n` is [`NonZeroU32`]: a zero requirement is not a quorum.
    Quorum(NonZeroU32),
    /// Require all currently always-on copies (including local).
    AllAlwaysOn,
    /// Durable only on the owner node.
    OwnerOnly,
}

impl QuorumPolicy {
    /// Need exactly `n` durable copies (local write counts as one).
    ///
    /// # Panics
    ///
    /// If `n == 0`. Use [`Self::OwnerOnly`] or [`Self::AllAlwaysOn`] when the
    /// need is not a fixed positive count.
    #[track_caller]
    pub const fn n(n: u32) -> Self {
        match NonZeroU32::new(n) {
            Some(nz) => Self::Quorum(nz),
            None => panic!("QuorumPolicy::n requires n >= 1"),
        }
    }

    /// `always_on` is reachable copies **including** the local write.
    pub fn required_acks(self, always_on: u32) -> u32 {
        match self {
            QuorumPolicy::Quorum(n) => n.get(),
            QuorumPolicy::AllAlwaysOn => always_on,
            QuorumPolicy::OwnerOnly => 1,
        }
    }

    /// `total_acks` is local (1) plus remote `replicate_*` return value.
    pub fn is_satisfied(self, total_acks: u32, always_on: u32) -> bool {
        total_acks >= self.required_acks(always_on)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use arkfs_test_review::{
        review_assert as assert, review_eq as assert_eq, review_ne as assert_ne,
    };

    /// OwnerOnly is satisfied by the local write alone.
    #[test]
    fn owner_only_needs_one() {
        let _g = arkfs_test_review::guard();
        assert_eq!(QuorumPolicy::OwnerOnly.required_acks(5), 1);
        assert!(QuorumPolicy::OwnerOnly.is_satisfied(1, 5));
    }

    /// AllAlwaysOn needs every reachable replica including local.
    #[test]
    fn all_always_on_tracks_live_set() {
        let _g = arkfs_test_review::guard();
        assert_eq!(QuorumPolicy::AllAlwaysOn.required_acks(3), 3);
        assert!(!QuorumPolicy::AllAlwaysOn.is_satisfied(2, 3));
        assert!(QuorumPolicy::AllAlwaysOn.is_satisfied(1, 1));
        assert!(QuorumPolicy::AllAlwaysOn.is_satisfied(2, 2));
    }

    /// Quorum(n) is the integer n, not min(n, always_on).
    #[test]
    fn quorum_n_is_not_capped_at_always_on() {
        let _g = arkfs_test_review::guard();
        assert_eq!(QuorumPolicy::n(5).required_acks(2), 5);
        assert!(!QuorumPolicy::n(5).is_satisfied(2, 2));
        assert!(!QuorumPolicy::n(3).is_satisfied(2, 3));
        assert!(QuorumPolicy::n(2).is_satisfied(2, 1));
        assert!(QuorumPolicy::n(1).is_satisfied(1, 1));
    }

    /// Zero is not a quorum policy.
    #[test]
    #[should_panic(expected = "n >= 1")]
    fn quorum_n_rejects_zero() {
        let _g = arkfs_test_review::guard();
        let _ = QuorumPolicy::n(0);
    }
}
