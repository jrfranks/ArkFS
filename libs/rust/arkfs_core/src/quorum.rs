//! How many durable copies count as “safe” for [`persistent_object_store`].
//!
//! The store always counts the **local** write as one ack. `always_on` passed
//! into these methods is reachable copies **including local**. Isolated
//! single-node (zero remotes) therefore has `always_on == 1`.
//!
//! `OwnerOnly` is the FUSE default. `Quorum(n)` with `n > 1` and no remotes
//! must fail closed — do not lower `n` silently.

use serde::{Deserialize, Serialize};

/// Replication / durability policy for safe-write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuorumPolicy {
    /// Require acknowledgment from `n` copies (local durable write counts as one).
    Quorum(u32),
    /// Require all currently always-on copies (including local).
    AllAlwaysOn,
    /// Durable only on the owner node.
    OwnerOnly,
}

impl QuorumPolicy {
    /// `always_on` is reachable copies **including** the local write.
    pub fn required_acks(self, always_on: u32) -> u32 {
        match self {
            QuorumPolicy::Quorum(n) => n,
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
    }
}
