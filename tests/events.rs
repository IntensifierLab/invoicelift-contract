//! Event-emission acceptance tests (issue #4).
//!
//! Each state-changing entrypoint is driven to its success path and the
//! emitted event is compared whole — topics and payload together. `Val` has no
//! `PartialEq`, but `soroban_sdk::Vec` compares with host-side deep equality,
//! so asserting on the whole event vec is both stricter and simpler than
//! picking fields out of it.
//!
//! In its own file rather than appended to `tests/integration.rs`, so this
//! change shares no region with other in-flight work.

use invoice_registry::{InvoiceRegistry, InvoiceRegistryClient};
use pool_manager::{PoolManager, PoolManagerClient};
use repayment_waterfall::{RepaymentWaterfall, RepaymentWaterfallClient};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events, Ledger},
    vec, Address, Env, IntoVal, Symbol,
};

const NOW: u64 = 5_000;

/// A test env with auths mocked and the ledger clock pinned, so an event
/// carrying a timestamp has a deterministic value to assert against.
///
/// `env.events().all()` is cumulative across a test, so assertions below take
/// the last event or search the buffer rather than comparing it whole.
fn env_at(now: u64) -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = now);
    env
}

// ── invoice-registry: verify_invoice ─────────────────────────────────────────

#[test]
fn verify_invoice_emits_who_verified_and_when() {
    let env = env_at(NOW);
    let id = env.register_contract(None::<&Address>, InvoiceRegistry);
    let registry = InvoiceRegistryClient::new(&env, &id);

    registry.initialize(&symbol_short!("admin"));
    registry.set_verifier(&symbol_short!("admin"), &symbol_short!("ver1"), &true);
    registry.register(&symbol_short!("inv1"), &42, &symbol_short!("sme1"));

    registry.verify_invoice(&symbol_short!("ver1"), &symbol_short!("inv1"));

    let events = env.events().all();
    let last = events.last().expect("verify_invoice should emit an event");
    assert_eq!(
        vec![&env, last],
        vec![
            &env,
            (
                id.clone(),
                (symbol_short!("inv_ver"), symbol_short!("inv1")).into_val(&env),
                (symbol_short!("ver1"), NOW).into_val(&env),
            ),
        ],
        "the id belongs in a topic so indexers can filter per invoice; the \
         payload must name the verifier and the time"
    );
}

// ── invoice-registry: assign_invoice ─────────────────────────────────────────

#[test]
fn assign_invoice_emits_the_owner_it_moved_from() {
    let env = env_at(NOW);
    let id = env.register_contract(None::<&Address>, InvoiceRegistry);
    let registry = InvoiceRegistryClient::new(&env, &id);

    registry.initialize(&symbol_short!("admin"));
    registry.set_verifier(&symbol_short!("admin"), &symbol_short!("ver1"), &true);
    registry.register(&symbol_short!("inv1"), &42, &symbol_short!("sme1"));
    registry.verify_invoice(&symbol_short!("ver1"), &symbol_short!("inv1"));

    let pool_manager = Address::generate(&env);
    registry.assign_invoice(&pool_manager, &symbol_short!("inv1"), &symbol_short!("pool1"));

    let events = env.events().all();
    let last = events.last().expect("assign_invoice should emit an event");
    assert_eq!(
        vec![&env, last],
        vec![
            &env,
            (
                id.clone(),
                (symbol_short!("inv_asgn"), symbol_short!("inv1")).into_val(&env),
                (symbol_short!("sme1"), symbol_short!("pool1"), NOW).into_val(&env),
            ),
        ],
        "the previous owner is only recoverable from the event — the write \
         that follows erases it from storage"
    );
}

// ── pool-manager: join_pool ──────────────────────────────────────────────────

#[test]
fn join_pool_emits_enough_to_reconcile_the_mint() {
    let env = env_at(NOW);
    let id = env.register_contract(None::<&Address>, PoolManager);
    let pool = PoolManagerClient::new(&env, &id);
    pool.initialize(&symbol_short!("admin"), &8_000);

    let lender = Address::generate(&env);
    let shares = pool.join_pool(&lender, &symbol_short!("lend1"), &1_000);

    let events = env.events().all();
    let last = events.last().expect("join_pool should emit an event");
    assert_eq!(
        vec![&env, last],
        vec![
            &env,
            (
                id.clone(),
                (symbol_short!("shr_mint"), lender.clone()).into_val(&env),
                (1_000i128, shares, pool.nav(), pool.total_shares()).into_val(&env),
            ),
        ],
        "shares alone cannot be reconciled: without the amount and the NAV \
         used there is no way to check the mint was priced correctly"
    );
}

#[test]
fn join_pool_event_carries_the_running_share_total() {
    // Two deposits: the second event must report the cumulative total, not
    // the second lender's own shares, or an indexer cannot track pool size.
    let env = env_at(NOW);
    let id = env.register_contract(None::<&Address>, PoolManager);
    let pool = PoolManagerClient::new(&env, &id);
    pool.initialize(&symbol_short!("admin"), &8_000);

    let first = Address::generate(&env);
    let second = Address::generate(&env);
    let first_shares = pool.join_pool(&first, &symbol_short!("lend1"), &1_000);
    let second_shares = pool.join_pool(&second, &symbol_short!("lend2"), &500);

    let events = env.events().all();
    let last = events.last().unwrap();
    assert_eq!(
        vec![&env, last],
        vec![
            &env,
            (
                id.clone(),
                (symbol_short!("shr_mint"), second.clone()).into_val(&env),
                (500i128, second_shares, pool.nav(), first_shares + second_shares).into_val(&env),
            ),
        ]
    );
}

// ── repayment-waterfall: process_repayment ───────────────────────────────────

#[test]
fn process_repayment_emits_an_event() {
    // This entrypoint previously emitted nothing at all, so a repayment was
    // invisible to any off-chain index.
    let env = env_at(NOW);

    let pool_id = env.register_contract(None::<&Address>, PoolManager);
    let waterfall_id = env.register_contract(None::<&Address>, RepaymentWaterfall);
    let pool = PoolManagerClient::new(&env, &pool_id);
    let waterfall = RepaymentWaterfallClient::new(&env, &waterfall_id);

    pool.initialize(&symbol_short!("admin"), &8_000);
    waterfall.initialize(&symbol_short!("admin"), &pool_id);

    waterfall.process_repayment(&750);

    // `Val` has no `PartialEq`, so each candidate is wrapped in a one-element
    // `soroban_sdk::Vec` and compared with host-side deep equality. The
    // waterfall calls into pool-manager, which emits events of its own, so
    // this searches rather than comparing the whole buffer.
    let events = env.events().all();
    let expected = (
        waterfall_id.clone(),
        (symbol_short!("repaid"),).into_val(&env),
        (750i128, NOW).into_val(&env),
    );
    assert!(
        events
            .iter()
            .any(|event| vec![&env, event] == vec![&env, expected.clone()]),
        "expected a `repaid` event carrying the amount and timestamp, got {events:?}"
    );
}

#[test]
fn a_rejected_repayment_emits_nothing() {
    // An event for a repayment that was refused would put a phantom credit
    // into every downstream index.
    let env = env_at(NOW);

    let pool_id = env.register_contract(None::<&Address>, PoolManager);
    let waterfall_id = env.register_contract(None::<&Address>, RepaymentWaterfall);
    PoolManagerClient::new(&env, &pool_id).initialize(&symbol_short!("admin"), &8_000);
    let waterfall = RepaymentWaterfallClient::new(&env, &waterfall_id);
    waterfall.initialize(&symbol_short!("admin"), &pool_id);

    let _ = waterfall.try_process_repayment(&0);

    let events = env.events().all();
    let repaid_topic: soroban_sdk::Vec<soroban_sdk::Val> =
        (symbol_short!("repaid"),).into_val(&env);
    assert!(
        !events.iter().any(|(_, topics, _)| topics == repaid_topic),
        "a rejected repayment must not emit, got {events:?}"
    );
}
