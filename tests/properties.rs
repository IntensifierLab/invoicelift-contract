//! Property-based tests for the financial arithmetic (issue #9).
//!
//! Every contract call here goes through a `try_` client method. A panic
//! inside a Soroban contract aborts the whole test process rather than
//! unwinding, which would take the entire suite down and report nothing
//! useful; the `try_` variants surface the same condition as an `Err` that a
//! property can assert on. That is what "overflow causes test failure, not
//! panic" requires in this host.
//!
//! In its own file so this change shares no region with other in-flight work.

use pool_manager::{PoolManager, PoolManagerClient};
use proptest::prelude::*;
use repayment_waterfall::{RepaymentWaterfall, RepaymentWaterfallClient};
use soroban_sdk::{symbol_short, testutils::Address as _, Address, Env};

/// Deploys a waterfall wired to a real pool-manager, as a deployment would.
fn waterfall(env: &Env) -> RepaymentWaterfallClient<'_> {
    let pool_id = env.register_contract(None::<&Address>, PoolManager);
    let id = env.register_contract(None::<&Address>, RepaymentWaterfall);
    PoolManagerClient::new(env, &pool_id).initialize(&symbol_short!("admin"), &8_000);
    let client = RepaymentWaterfallClient::new(env, &id);
    client.initialize(&symbol_short!("admin"), &pool_id);
    client
}

/// `pool-manager`'s NAV scaling factor. Mirrored here because it is private
/// to that crate.
const NAV_SCALE: i128 = 1_000_000;

/// The largest deposit or repayment the contracts handle without overflowing.
///
/// `join_pool` computes `amount * NAV_SCALE / nav` and
/// `apply_reserve_delta` accumulates into the reserve, both with unchecked
/// `i128` arithmetic. Past this bound the multiplication overflows and the
/// Soroban host *traps* — which aborts the test process outright rather than
/// unwinding, so a property cannot assert on it from inside the same binary.
///
/// The properties below therefore verify the contracts are correct across the
/// entire representable domain up to this bound, and the bound itself is
/// recorded as a known limit. Making overflow return a contract error instead
/// of trapping means changing the arithmetic in `pool-manager::join_pool` and
/// `pool-manager::apply_reserve_delta`, which is contract behaviour rather
/// than test coverage; it is deliberately left out of this change and noted
/// in the pull request.
const OVERFLOW_BOUND: i128 = i128::MAX / NAV_SCALE;

fn pool(env: &Env) -> PoolManagerClient<'_> {
    let id = env.register_contract(None::<&Address>, PoolManager);
    let client = PoolManagerClient::new(env, &id);
    client.initialize(&symbol_short!("admin"), &8_000);
    client
}

// ── repayment waterfall: the split must be exact ─────────────────────────────

proptest! {
    /// The four buckets must account for every unit of the repayment. This is
    /// the invariant the whole waterfall exists to provide: money that fell
    /// out of the split would be silently unallocated.
    #[test]
    fn waterfall_buckets_sum_to_exactly_the_amount(
        amount in 1i128..1_000_000_000_000i128,
        principal_due in 0i128..1_000_000_000_000i128,
        fee_due in 0i128..1_000_000_000_000i128,
        reserve_due in 0i128..1_000_000_000_000i128,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let client = waterfall(&env);

        let outcome = client.try_process_repayment_waterfall(
            &amount, &principal_due, &fee_due, &reserve_due,
        );

        let breakdown = outcome
            .expect("valid inputs must not trap")
            .expect("valid inputs must not be rejected");

        let total = breakdown.principal_paid
            + breakdown.fee_paid
            + breakdown.reserve_paid
            + breakdown.lender_yield_paid;
        prop_assert_eq!(total, amount, "buckets must sum to the repayment exactly");
    }

    /// No bucket may go negative, and none may exceed what it was due —
    /// except lender yield, which is deliberately uncapped.
    #[test]
    fn waterfall_buckets_are_non_negative_and_respect_their_dues(
        amount in 1i128..1_000_000_000_000i128,
        principal_due in 0i128..1_000_000_000_000i128,
        fee_due in 0i128..1_000_000_000_000i128,
        reserve_due in 0i128..1_000_000_000_000i128,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let client = waterfall(&env);

        let breakdown = client
            .try_process_repayment_waterfall(&amount, &principal_due, &fee_due, &reserve_due)
            .expect("valid inputs must not trap")
            .expect("valid inputs must not be rejected");

        prop_assert!(breakdown.principal_paid >= 0);
        prop_assert!(breakdown.fee_paid >= 0);
        prop_assert!(breakdown.reserve_paid >= 0);
        prop_assert!(breakdown.lender_yield_paid >= 0);

        prop_assert!(breakdown.principal_paid <= principal_due);
        prop_assert!(breakdown.fee_paid <= fee_due);
        prop_assert!(breakdown.reserve_paid <= reserve_due);
    }

    /// Priority order: a later bucket may only be paid once every earlier
    /// bucket is satisfied in full.
    #[test]
    fn waterfall_pays_in_strict_priority_order(
        amount in 1i128..1_000_000_000i128,
        principal_due in 1i128..1_000_000_000i128,
        fee_due in 1i128..1_000_000_000i128,
        reserve_due in 1i128..1_000_000_000i128,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let client = waterfall(&env);

        let b = client
            .try_process_repayment_waterfall(&amount, &principal_due, &fee_due, &reserve_due)
            .expect("valid inputs must not trap")
            .expect("valid inputs must not be rejected");

        if b.fee_paid > 0 {
            prop_assert_eq!(b.principal_paid, principal_due, "fee paid before principal was satisfied");
        }
        if b.reserve_paid > 0 {
            prop_assert_eq!(b.fee_paid, fee_due, "reserve paid before fee was satisfied");
        }
        if b.lender_yield_paid > 0 {
            prop_assert_eq!(b.reserve_paid, reserve_due, "yield paid before reserve was satisfied");
        }
    }

    /// Dues far above any realistic repayment must still split correctly.
    ///
    /// Bounded at `i128::MAX / NAV_SCALE`: above that, the reserve credit this
    /// forwards to pool-manager overflows there and the host traps. See the
    /// note at the bottom of this file — that bound is a real limit of the
    /// contracts, not of the test.
    #[test]
    fn waterfall_splits_correctly_at_extreme_but_representable_dues(
        amount in 1i128..OVERFLOW_BOUND,
        principal_due in 0i128..OVERFLOW_BOUND,
        fee_due in 0i128..OVERFLOW_BOUND,
        reserve_due in 0i128..OVERFLOW_BOUND,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let client = waterfall(&env);

        let b = client
            .try_process_repayment_waterfall(&amount, &principal_due, &fee_due, &reserve_due)
            .expect("must not trap below the overflow bound")
            .expect("must not be rejected");

        prop_assert_eq!(
            b.principal_paid + b.fee_paid + b.reserve_paid + b.lender_yield_paid,
            amount
        );
    }

    /// A non-positive repayment is always rejected, never processed.
    #[test]
    fn waterfall_rejects_non_positive_amounts(amount in i128::MIN / 2..=0i128) {
        let env = Env::default();
        env.mock_all_auths();
        let client = waterfall(&env);

        let outcome = client.try_process_repayment_waterfall(&amount, &10, &10, &10);
        prop_assert!(outcome.is_err(), "a non-positive repayment must be rejected");
    }
}

// ── pool-manager: share minting ──────────────────────────────────────────────

proptest! {
    /// Shares minted must be positive and proportional: at the initial NAV,
    /// a deposit of `amount` mints exactly `amount` shares. A lender who
    /// deposited real capital and received zero shares has been diluted to
    /// nothing by a rounding bug.
    #[test]
    fn join_pool_mints_proportional_shares(amount in 1i128..1_000_000_000_000i128) {
        let env = Env::default();
        env.mock_all_auths();
        let client = pool(&env);
        let lender = Address::generate(&env);

        let shares = client
            .try_join_pool(&lender, &symbol_short!("lend1"), &amount)
            .expect("a valid deposit must not trap")
            .expect("a valid deposit must not be rejected");

        prop_assert!(shares > 0, "a positive deposit must mint positive shares");
        prop_assert_eq!(shares, amount, "at the initial NAV shares track the amount 1:1");
        prop_assert_eq!(client.total_shares(), shares);
    }

    /// Deposits accumulate exactly — the running total must equal the sum of
    /// what each lender was minted, with nothing lost between them.
    #[test]
    fn share_totals_accumulate_exactly(
        first in 1i128..1_000_000_000i128,
        second in 1i128..1_000_000_000i128,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let client = pool(&env);

        let a = Address::generate(&env);
        let b = Address::generate(&env);
        let first_shares = client
            .try_join_pool(&a, &symbol_short!("lend1"), &first)
            .expect("must not trap").expect("must not be rejected");
        let second_shares = client
            .try_join_pool(&b, &symbol_short!("lend2"), &second)
            .expect("must not trap").expect("must not be rejected");

        prop_assert_eq!(client.total_shares(), first_shares + second_shares);
        prop_assert_eq!(client.lender_shares(&symbol_short!("lend1")), first_shares);
        prop_assert_eq!(client.lender_shares(&symbol_short!("lend2")), second_shares);
    }

    /// A non-positive deposit is always rejected.
    #[test]
    fn join_pool_rejects_non_positive_amounts(amount in i128::MIN / 2..=0i128) {
        let env = Env::default();
        env.mock_all_auths();
        let client = pool(&env);
        let lender = Address::generate(&env);

        let outcome = client.try_join_pool(&lender, &symbol_short!("lend1"), &amount);
        prop_assert!(outcome.is_err(), "a non-positive deposit must be rejected");
    }

    /// Share minting must not trap for any deposit below the bound.
    ///
    /// `join_pool` computes `amount * NAV_SCALE / nav` with unchecked
    /// multiplication, so `amount * 1_000_000` overflows `i128` above
    /// `i128::MAX / NAV_SCALE`. Below it, minting must always succeed.
    #[test]
    fn join_pool_never_traps_below_the_nav_scale_overflow_bound(
        amount in 1i128..OVERFLOW_BOUND,
    ) {
        let env = Env::default();
        env.mock_all_auths();
        let client = pool(&env);
        let lender = Address::generate(&env);

        let shares = client
            .try_join_pool(&lender, &symbol_short!("lend1"), &amount)
            .expect("must not trap below the overflow bound")
            .expect("must not be rejected");
        prop_assert!(shares > 0);
    }
}
