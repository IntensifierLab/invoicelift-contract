//! Storage-key enum for the governance module — kept in its own module for
//! consistency with the other crates in this workspace, even though
//! `#[contracttype] enum DataKey` is already the type-safe alternative to an
//! inline string literal per key (a typo here is a compile error, not a
//! silent new storage slot).

use soroban_sdk::{contracttype, Address, Symbol};

/// Storage keys for the governance module's instance/persistent state.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum DataKey {
    /// The administrator, authorized to configure voting power (`set_voting_power`).
    Admin,
    /// Configurable voting period, in seconds, applied to every new proposal.
    VotingPeriodSecs,
    /// Minimum share of total voting power that must be cast, in basis points
    /// (10_000 = 100%), for a proposal's outcome to count.
    QuorumBps,
    /// Minimum share of *votes cast* that must be in favour for a
    /// [`ProposalKind::Critical`] proposal to pass, in basis points.
    SupermajorityBps,
    /// Sum of all voting power ever granted via `set_voting_power`.
    TotalPower,
    /// The next proposal id to be assigned.
    NextProposalId,
    /// A voter's current voting power (LP-share weight). Set by the admin —
    /// see the module docs for why this is a registry rather than a live read
    /// of `pool-manager` LP share balances.
    VoterPower(Address),
    /// The stored [`Proposal`] for a given id.
    Proposal(u32),
    /// Whether `voter` has already cast a vote on proposal `id`.
    Voted(u32, Address),
    /// The current effective value of a governed pool parameter, applied by
    /// `execute_proposal` once a proposal targeting it passes.
    Parameter(Symbol),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each no-argument key variant is distinct — an accidental duplicate
    /// variant (or a copy-paste that reintroduces a removed one under a new
    /// name) would otherwise be a silent storage-slot collision once these
    /// are written to the ledger.
    #[test]
    fn no_arg_key_variants_are_pairwise_distinct() {
        let variants = [
            DataKey::Admin,
            DataKey::VotingPeriodSecs,
            DataKey::QuorumBps,
            DataKey::SupermajorityBps,
            DataKey::TotalPower,
            DataKey::NextProposalId,
        ];
        for (i, a) in variants.iter().enumerate() {
            for (j, b) in variants.iter().enumerate() {
                assert_eq!(
                    i == j,
                    a == b,
                    "variants at {i} and {j} should only be equal to themselves"
                );
            }
        }
    }
}
