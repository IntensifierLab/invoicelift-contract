#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, Symbol,
};

/// Errors surfaced by the governance module. Stable `u32` discriminants so
/// integrators and audit tooling can match on them.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// `initialize` called on an already-initialized contract.
    AlreadyInitialized = 1,
    /// An admin-guarded or proposal call was made before `initialize`.
    NotInitialized = 2,
    /// `quorum_bps` or `supermajority_bps` was outside `(0, 10_000]`, or
    /// `supermajority_bps` was not a strict majority (`> 5_000`).
    InvalidConfig = 3,
    /// The referenced proposal id does not exist.
    ProposalNotFound = 4,
    /// The caller holds zero voting power, so cannot propose or vote.
    NoVotingPower = 5,
    /// `vote` was called after the proposal's voting window has closed.
    VotingClosed = 6,
    /// `execute_proposal` was called before the voting window has closed.
    VotingStillOpen = 7,
    /// `voter` has already cast a vote on this proposal.
    AlreadyVoted = 8,
    /// The proposal has already been executed (or finalized as rejected).
    AlreadyFinalized = 9,
}

/// Storage keys for the governance module's instance/persistent state.
#[contracttype]
#[derive(Clone)]
enum DataKey {
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

/// Whether a proposal only needs a simple majority of votes cast
/// ([`ProposalKind::Standard`]), or the configured supermajority
/// ([`ProposalKind::Critical`]) — used for changes to sensitive parameters
/// (e.g. utilisation ceilings) where a slim majority is too easy to reverse.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProposalKind {
    Standard,
    Critical,
}

/// A proposal to set a governed pool parameter (identified by `param`, an
/// arbitrary key such as `max_util`) to `new_value`, once voting passes.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub id: u32,
    pub proposer: Address,
    pub kind: ProposalKind,
    pub param: Symbol,
    pub new_value: i128,
    pub created_at: u64,
    pub voting_end: u64,
    pub votes_for: i128,
    pub votes_against: i128,
    pub executed: bool,
}

/// Basis-points scale (10_000 = 100%).
const BPS_SCALE: u32 = 10_000;
/// The minimum passing share for a [`ProposalKind::Standard`] proposal: a
/// strict majority of votes cast.
const STANDARD_THRESHOLD_BPS: u32 = 5_000;

/// Lightweight on-chain governance for pool parameter changes.
///
/// LP token holders (represented here as [`Address`]es with a voting-power
/// weight) propose and vote on changes to named pool parameters. A proposal
/// auto-executes — applying its `new_value` and marking itself finalized —
/// the moment its tally clears both quorum and the required majority; no
/// separate approval step is needed. `execute_proposal` exists to finalize a
/// proposal once its voting window has elapsed without a decisive vote
/// (e.g. it never reached quorum), so it can't be voted on again.
///
/// Voting power is a registry set via `set_voting_power` rather than a live
/// read of `pool-manager`'s LP share balances: `pool-manager` identifies
/// lenders by `Symbol`, not `Address` (see its `LenderKey`), so there is no
/// direct address-keyed share balance to read cross-contract today. Wiring
/// this to real LP shares is a natural follow-up once that identity model is
/// unified; until then, the registry lets this contract be deployed and voted
/// on independently.
#[contract]
pub struct Governance;

#[contractimpl]
impl Governance {
    /// One-time initialization. `voting_period_secs` is how long each new
    /// proposal stays open to votes. `quorum_bps` and `supermajority_bps` are
    /// basis points in `(0, 10_000]`; `supermajority_bps` must additionally
    /// exceed `5_000` (a "supermajority" that isn't a majority is a
    /// contradiction).
    pub fn initialize(
        env: Env,
        admin: Address,
        voting_period_secs: u64,
        quorum_bps: u32,
        supermajority_bps: u32,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        if quorum_bps == 0
            || quorum_bps > BPS_SCALE
            || supermajority_bps <= STANDARD_THRESHOLD_BPS
            || supermajority_bps > BPS_SCALE
        {
            return Err(Error::InvalidConfig);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::VotingPeriodSecs, &voting_period_secs);
        env.storage()
            .instance()
            .set(&DataKey::QuorumBps, &quorum_bps);
        env.storage()
            .instance()
            .set(&DataKey::SupermajorityBps, &supermajority_bps);
        env.storage().instance().set(&DataKey::TotalPower, &0_i128);
        env.storage()
            .instance()
            .set(&DataKey::NextProposalId, &0_u32);
        Ok(())
    }

    /// Admin-only: sets `voter`'s voting power (replacing any prior value).
    /// Requires the admin's authorization.
    pub fn set_voting_power(env: Env, voter: Address, power: i128) -> Result<(), Error> {
        assert!(power >= 0, "power must not be negative");
        let admin = Self::read_admin(&env)?;
        admin.require_auth();

        let key = DataKey::VoterPower(voter);
        let previous: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &power);

        let total: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalPower)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::TotalPower, &(total - previous + power));
        Ok(())
    }

    /// Creates a new proposal to set `param` to `new_value` if it passes.
    /// The proposer must hold nonzero voting power and authorize the call.
    /// Returns the new proposal's id.
    pub fn create_proposal(
        env: Env,
        proposer: Address,
        kind: ProposalKind,
        param: Symbol,
        new_value: i128,
    ) -> Result<u32, Error> {
        proposer.require_auth();
        // read_admin doubles as the "has `initialize` run?" guard, since every
        // instance key below it is set atomically in the same call.
        Self::read_admin(&env)?;
        if Self::get_voting_power(env.clone(), proposer.clone()) == 0 {
            return Err(Error::NoVotingPower);
        }

        let id: u32 = env
            .storage()
            .instance()
            .get(&DataKey::NextProposalId)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::NextProposalId, &(id + 1));

        let period: u64 = env
            .storage()
            .instance()
            .get(&DataKey::VotingPeriodSecs)
            .unwrap_or(0);
        let created_at = env.ledger().timestamp();

        let proposal = Proposal {
            id,
            proposer: proposer.clone(),
            kind,
            param: param.clone(),
            new_value,
            created_at,
            voting_end: created_at + period,
            votes_for: 0,
            votes_against: 0,
            executed: false,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(id), &proposal);

        env.events().publish(
            (symbol_short!("proposed"), id, proposer),
            (param, new_value),
        );
        Ok(id)
    }

    /// Casts `voter`'s full voting power as `support`/against on `proposal_id`.
    /// Requires `voter`'s authorization. If this vote pushes the tally past
    /// both quorum and the required majority, the proposal auto-executes in
    /// this same call — see the type-level docs. Returns whether it did.
    pub fn vote(env: Env, proposal_id: u32, voter: Address, support: bool) -> Result<bool, Error> {
        voter.require_auth();

        let power = Self::get_voting_power(env.clone(), voter.clone());
        if power == 0 {
            return Err(Error::NoVotingPower);
        }

        let mut proposal = Self::read_proposal(&env, proposal_id)?;
        if proposal.executed {
            return Err(Error::AlreadyFinalized);
        }
        if env.ledger().timestamp() >= proposal.voting_end {
            return Err(Error::VotingClosed);
        }

        let voted_key = DataKey::Voted(proposal_id, voter.clone());
        if env.storage().persistent().has(&voted_key) {
            return Err(Error::AlreadyVoted);
        }
        env.storage().persistent().set(&voted_key, &true);

        if support {
            proposal.votes_for += power;
        } else {
            proposal.votes_against += power;
        }

        let executed = Self::try_execute(&env, &mut proposal)?;
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);

        env.events().publish(
            (symbol_short!("voted"), proposal_id, voter),
            (support, power, executed),
        );
        Ok(executed)
    }

    /// Finalizes `proposal_id` once its voting window has closed: applies its
    /// change if it passed, or simply leaves it un-executed (rejected) if it
    /// didn't. Idempotent — re-calling an already-executed proposal returns
    /// `Ok(true)` without effect. Callable by anyone, since the outcome is
    /// fully determined by the recorded tally and the current time.
    pub fn execute_proposal(env: Env, proposal_id: u32) -> Result<bool, Error> {
        let mut proposal = Self::read_proposal(&env, proposal_id)?;
        if proposal.executed {
            return Ok(true);
        }
        if env.ledger().timestamp() < proposal.voting_end {
            return Err(Error::VotingStillOpen);
        }

        let executed = Self::try_execute(&env, &mut proposal)?;
        env.storage()
            .persistent()
            .set(&DataKey::Proposal(proposal_id), &proposal);
        Ok(executed)
    }

    // ── view helpers ──────────────────────────────────────────────────

    pub fn get_proposal(env: Env, proposal_id: u32) -> Result<Proposal, Error> {
        Self::read_proposal(&env, proposal_id)
    }

    pub fn get_voting_power(env: Env, voter: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::VoterPower(voter))
            .unwrap_or(0)
    }

    pub fn total_voting_power(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::TotalPower)
            .unwrap_or(0)
    }

    /// The current effective value of a governed parameter, or `None` if no
    /// proposal targeting it has ever passed.
    pub fn get_parameter(env: Env, param: Symbol) -> Option<i128> {
        env.storage().instance().get(&DataKey::Parameter(param))
    }

    /// Contract ABI / deployment marker for integrators.
    pub fn version(_env: Env) -> u32 {
        1
    }

    // ── internal helpers ──────────────────────────────────────────────

    fn read_admin(env: &Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
    }

    fn read_proposal(env: &Env, proposal_id: u32) -> Result<Proposal, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Proposal(proposal_id))
            .ok_or(Error::ProposalNotFound)
    }

    /// Evaluates `proposal`'s tally against quorum and its required majority.
    /// If it passes, writes `Parameter(param)`, marks `executed`, and emits
    /// `executed`; a failing tally is left as-is (not marked executed) so a
    /// still-open proposal can keep collecting votes. Mutates `proposal` in
    /// place; the caller is responsible for persisting it.
    fn try_execute(env: &Env, proposal: &mut Proposal) -> Result<bool, Error> {
        let total_power: i128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalPower)
            .unwrap_or(0);
        let cast = proposal.votes_for + proposal.votes_against;
        if total_power == 0 || cast == 0 {
            return Ok(false);
        }

        let quorum_bps: i128 = env
            .storage()
            .instance()
            .get::<DataKey, u32>(&DataKey::QuorumBps)
            .unwrap_or(BPS_SCALE) as i128;
        let quorum_met = cast * (BPS_SCALE as i128) >= quorum_bps * total_power;
        if !quorum_met {
            return Ok(false);
        }

        let required_bps: i128 = match proposal.kind {
            ProposalKind::Standard => (STANDARD_THRESHOLD_BPS as i128) + 1,
            ProposalKind::Critical => env
                .storage()
                .instance()
                .get::<DataKey, u32>(&DataKey::SupermajorityBps)
                .unwrap_or(BPS_SCALE) as i128,
        };
        let passed = proposal.votes_for * (BPS_SCALE as i128) >= required_bps * cast;
        if !passed {
            return Ok(false);
        }

        env.storage().instance().set(
            &DataKey::Parameter(proposal.param.clone()),
            &proposal.new_value,
        );
        proposal.executed = true;
        env.events().publish(
            (symbol_short!("executed"), proposal.id),
            (proposal.param.clone(), proposal.new_value),
        );
        Ok(true)
    }
}

#[cfg(test)]
mod test;
