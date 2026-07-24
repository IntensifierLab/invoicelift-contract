extern crate std;

use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _},
    Address, Env,
};

use crate::{Error, Governance, GovernanceClient, ProposalKind};

/// quorum 3_000 bps (30%), supermajority 6_600 bps (66%), 7-day voting window.
const VOTING_PERIOD: u64 = 7 * 24 * 60 * 60;
const QUORUM_BPS: u32 = 3_000;
const SUPERMAJORITY_BPS: u32 = 6_600;

fn setup<'a>(env: &'a Env) -> (GovernanceClient<'a>, Address) {
    let contract_id = env.register_contract(None, Governance);
    let client = GovernanceClient::new(env, &contract_id);
    let admin = Address::generate(env);
    client.initialize(&admin, &VOTING_PERIOD, &QUORUM_BPS, &SUPERMAJORITY_BPS);
    (client, admin)
}

fn advance_past_voting_end(env: &Env) {
    let target = env.ledger().timestamp() + VOTING_PERIOD + 1;
    env.ledger().with_mut(|li| li.timestamp = target);
}

#[test]
fn initialize_sets_config() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    assert_eq!(client.total_voting_power(), 0);
}

#[test]
fn initialize_twice_errors() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);
    let other = Address::generate(&env);
    assert_eq!(
        client.try_initialize(&other, &VOTING_PERIOD, &QUORUM_BPS, &SUPERMAJORITY_BPS),
        Err(Ok(Error::AlreadyInitialized))
    );
}

#[test]
fn initialize_rejects_invalid_config() {
    let env = Env::default();
    let contract_id = env.register_contract(None, Governance);
    let client = GovernanceClient::new(&env, &contract_id);
    let admin = Address::generate(&env);

    // quorum of 0 is meaningless.
    assert_eq!(
        client.try_initialize(&admin, &VOTING_PERIOD, &0, &SUPERMAJORITY_BPS),
        Err(Ok(Error::InvalidConfig))
    );
    // a "supermajority" at or below a simple majority is a contradiction.
    assert_eq!(
        client.try_initialize(&admin, &VOTING_PERIOD, &QUORUM_BPS, &5_000),
        Err(Ok(Error::InvalidConfig))
    );
}

#[test]
fn set_voting_power_sets_power_and_total() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);
    let alice = Address::generate(&env);

    client.set_voting_power(&alice, &100);
    assert_eq!(client.get_voting_power(&alice), 100);
    assert_eq!(client.total_voting_power(), 100);
}

#[test]
fn set_voting_power_replaces_prior_value_in_total() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);
    let alice = Address::generate(&env);

    client.set_voting_power(&alice, &100);
    client.set_voting_power(&alice, &40);

    assert_eq!(client.get_voting_power(&alice), 40);
    assert_eq!(client.total_voting_power(), 40);
}

#[test]
fn create_proposal_requires_voting_power() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);
    let outsider = Address::generate(&env);

    let param = symbol_short!("max_util");
    assert_eq!(
        client.try_create_proposal(&outsider, &ProposalKind::Standard, &param, &9_000),
        Err(Ok(Error::NoVotingPower))
    );
}

#[test]
fn standard_proposal_auto_executes_on_majority_and_quorum() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);

    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let carol = Address::generate(&env);
    // Total power 100; quorum 30% = 30 must be cast.
    client.set_voting_power(&alice, &50);
    client.set_voting_power(&bob, &30);
    client.set_voting_power(&carol, &20);

    let param = symbol_short!("max_util");
    let id = client.create_proposal(&alice, &ProposalKind::Standard, &param, &9_000);

    // Alice (50) votes for — meets quorum (50 >= 30) and majority (100% for).
    let executed = client.vote(&id, &alice, &true);
    assert!(
        executed,
        "should auto-execute once quorum + majority are met"
    );

    let proposal = client.get_proposal(&id);
    assert!(proposal.executed);
    assert_eq!(client.get_parameter(&param), Some(9_000));

    // A late vote from bob after execution is rejected.
    assert_eq!(
        client.try_vote(&id, &bob, &true),
        Err(Ok(Error::AlreadyFinalized))
    );
}

#[test]
fn proposal_does_not_execute_before_quorum_met() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);

    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    // Total power 1_000; quorum 30% = 300.
    client.set_voting_power(&alice, &100);
    client.set_voting_power(&bob, &900);

    let param = symbol_short!("max_util");
    let id = client.create_proposal(&alice, &ProposalKind::Standard, &param, &9_000);

    // Alice's 100 votes for is 100% in favour, but only 10% of total power —
    // below the 30% quorum, so it must not execute yet.
    let executed = client.vote(&id, &alice, &true);
    assert!(!executed);
    assert_eq!(client.get_parameter(&param), None);

    // Bob's 900 pushes cast power to 1_000 (100% >= 30% quorum) and is also
    // in favour, so this vote crosses both bars and auto-executes.
    let executed = client.vote(&id, &bob, &true);
    assert!(executed);
    assert_eq!(client.get_parameter(&param), Some(9_000));
}

#[test]
fn critical_proposal_needs_supermajority_not_just_majority() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);

    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    // Total power 100; quorum 30%.
    client.set_voting_power(&alice, &60);
    client.set_voting_power(&bob, &40);

    let param = symbol_short!("max_util");
    let id = client.create_proposal(&alice, &ProposalKind::Critical, &param, &9_500);

    // 60-for/40-against clears quorum (100% cast) and is a simple majority
    // (60%), but falls short of the 66% supermajority this Critical proposal
    // requires — must not execute.
    client.vote(&id, &bob, &false);
    let executed = client.vote(&id, &alice, &true);
    assert!(
        !executed,
        "60% for must not satisfy a 66% supermajority bar"
    );
    assert_eq!(client.get_parameter(&param), None);

    // Once voting closes without reaching supermajority, it's rejected —
    // execute_proposal finalizes it as not-executed rather than erroring.
    advance_past_voting_end(&env);
    let executed = client.execute_proposal(&id);
    assert!(!executed);
    assert!(!client.get_proposal(&id).executed);
}

#[test]
fn critical_proposal_executes_at_exact_supermajority() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);

    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    // 66-for / 34-against = exactly the 66% supermajority bar, 100% quorum.
    client.set_voting_power(&alice, &66);
    client.set_voting_power(&bob, &34);

    let param = symbol_short!("max_util");
    let id = client.create_proposal(&alice, &ProposalKind::Critical, &param, &9_500);

    client.vote(&id, &bob, &false);
    let executed = client.vote(&id, &alice, &true);
    assert!(
        executed,
        "exactly 66% for must clear a 66% supermajority bar"
    );
    assert_eq!(client.get_parameter(&param), Some(9_500));
}

#[test]
fn cannot_vote_twice_on_the_same_proposal() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);

    let alice = Address::generate(&env);
    client.set_voting_power(&alice, &10);
    let bob = Address::generate(&env);
    client.set_voting_power(&bob, &10_000);

    let param = symbol_short!("max_util");
    let id = client.create_proposal(&alice, &ProposalKind::Standard, &param, &9_000);

    client.vote(&id, &alice, &true);
    assert_eq!(
        client.try_vote(&id, &alice, &false),
        Err(Ok(Error::AlreadyVoted))
    );
}

#[test]
fn cannot_vote_after_voting_window_closes() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);

    let alice = Address::generate(&env);
    client.set_voting_power(&alice, &10);

    let param = symbol_short!("max_util");
    let id = client.create_proposal(&alice, &ProposalKind::Standard, &param, &9_000);

    advance_past_voting_end(&env);
    assert_eq!(
        client.try_vote(&id, &alice, &true),
        Err(Ok(Error::VotingClosed))
    );
}

#[test]
fn execute_proposal_before_voting_end_errors() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);

    let alice = Address::generate(&env);
    client.set_voting_power(&alice, &10);
    let param = symbol_short!("max_util");
    let id = client.create_proposal(&alice, &ProposalKind::Standard, &param, &9_000);

    assert_eq!(
        client.try_execute_proposal(&id),
        Err(Ok(Error::VotingStillOpen))
    );
}

#[test]
fn execute_proposal_is_idempotent_after_auto_execution() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);

    let alice = Address::generate(&env);
    client.set_voting_power(&alice, &10_000);
    let param = symbol_short!("max_util");
    let id = client.create_proposal(&alice, &ProposalKind::Standard, &param, &9_000);

    let executed = client.vote(&id, &alice, &true);
    assert!(executed);

    advance_past_voting_end(&env);
    // Re-finalizing an already-executed proposal is a no-op that reports success.
    assert!(client.execute_proposal(&id));
}

#[test]
fn get_proposal_missing_id_errors() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);
    assert_eq!(
        client.try_get_proposal(&999),
        Err(Ok(Error::ProposalNotFound))
    );
}

#[test]
fn version_is_stable() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    assert_eq!(client.version(), 1);
}
