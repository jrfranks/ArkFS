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

    pub fn is_satisfied(self, total_acks: u32, always_on: u32) -> bool {
        total_acks >= self.required_acks(always_on)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_only_needs_one() {
        assert_eq!(QuorumPolicy::OwnerOnly.required_acks(5), 1);
        assert!(QuorumPolicy::OwnerOnly.is_satisfied(1, 5));
    }

    #[test]
    fn all_always_on_tracks_live_set() {
        assert_eq!(QuorumPolicy::AllAlwaysOn.required_acks(3), 3);
        assert!(!QuorumPolicy::AllAlwaysOn.is_satisfied(2, 3));
    }
}
