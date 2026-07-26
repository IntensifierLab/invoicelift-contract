#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Env, Symbol, Vec};

/// Pedersen constants matching those in the invoice registry.
const P: i128 = i128::MAX;

#[allow(dead_code)]
const G: i128 = 5;
#[allow(dead_code)]
const H: i128 = 7;

/// Helper to reduce a u128 modulo P = 2^127 - 1 using Mersenne prime properties.
fn reduce_u128(x: u128) -> u128 {
    const P_U128: u128 = (1 << 127) - 1;
    let mut val = (x & P_U128) + (x >> 127);
    if val >= P_U128 {
        val -= P_U128;
    }
    val
}

/// Modular multiplication safe for i128 and Mersenne prime P = 2^127 - 1.
fn mod_mul(a: i128, b: i128, p: i128) -> i128 {
    let a = a.rem_euclid(p) as u128;
    let b = b.rem_euclid(p) as u128;

    let a_lo = a & 0xFFFF_FFFF_FFFF_FFFF;
    let a_hi = a >> 64;
    let b_lo = b & 0xFFFF_FFFF_FFFF_FFFF;
    let b_hi = b >> 64;

    let term0 = a_lo * b_lo;
    let term1_1 = a_lo * b_hi;
    let term1_2 = a_hi * b_lo;
    let term2 = a_hi * b_hi;

    let r0 = reduce_u128(term0);

    let r1_1 = reduce_u128(term1_1);
    let r1_2 = reduce_u128(term1_2);
    let r1 = reduce_u128(r1_1 + r1_2);

    let r1_lo = r1 & 0x7FFF_FFFF_FFFF_FFFF;
    let r1_hi = r1 >> 63;
    let r1_scaled = reduce_u128((r1_lo << 64) + r1_hi);

    let r2 = reduce_u128(term2);
    let r2_scaled = reduce_u128(r2 << 1);

    let sum1 = reduce_u128(r0 + r1_scaled);
    let total = reduce_u128(sum1 + r2_scaled);
    total as i128
}

/// Extended Euclidean algorithm for modular inverse.
fn mod_inverse(a: i128, m: i128) -> i128 {
    let a = a.rem_euclid(m);
    let (mut old_r, mut r) = (a, m);
    let (mut old_s, mut s) = (1_i128, 0_i128);

    while r != 0 {
        let q = old_r / r;
        let tmp_r = r;
        r = old_r - q * r;
        old_r = tmp_r;

        let tmp_s = s;
        s = old_s - q * s;
        old_s = tmp_s;
    }

    assert!(old_r == 1, "no modular inverse exists");
    old_s.rem_euclid(m)
}

/// Scale a commitment mod P by a rational factor `scalar / scale`.
fn scale_commitment(c: i128, scalar: i128, scale: i128) -> i128 {
    let scaled = mod_mul(c, scalar, P);
    let inv = mod_inverse(scale, P);
    mod_mul(scaled, inv, P)
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaterfallTier {
    pub recipient: Symbol,
    pub share_bps: i128,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaterfallResult {
    pub recipient: Symbol,
    pub commitment: i128,
}

use soroban_sdk::{contractclient, Address};

const POOL_MANAGER: Symbol = symbol_short!("pool_mgr");

/// Minimal cross-contract interface onto `pool-manager`'s reserve-delta entry
/// point. Declared locally (rather than depending on the `pool-manager` crate
/// directly) because linking a sibling `#[contract]` crate's rlib into this
/// contract's Wasm re-exports its `#[contractimpl]` functions too, colliding
/// with this contract's own exports of the same names (e.g. `initialize`,
/// `version`) at link time.
#[contractclient(name = "PoolManagerClient")]
pub trait PoolManagerInterface {
    fn apply_reserve_delta(env: Env, delta: i128);
}

/// Priority repayment routing.
#[contract]
pub struct RepaymentWaterfall;

#[contractimpl]
impl RepaymentWaterfall {
    /// One-time initialization (scaffold — replace with auth in production).
    /// `pool_manager` is the deployed PoolManager contract this waterfall
    /// forwards repayments to.
    pub fn initialize(env: Env, admin: Symbol, pool_manager: Address) {
        if env.storage().instance().has(&symbol_short!("admin")) {
            panic!("already initialized");
        }
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &admin);
        env.storage().instance().set(&POOL_MANAGER, &pool_manager);
    }

    /// Split a total confidential commitment across multiple tiers according to basis points.
    /// The sum of the split commitments is guaranteed to match the total commitment homomorphically.
    pub fn split_commitment(
        _env: Env,
        total_commitment: i128,
        tiers: Vec<WaterfallTier>,
    ) -> Vec<WaterfallResult> {
        let mut results = Vec::new(&_env);
        let mut sum_bps: i128 = 0;

        for tier in tiers.iter() {
            assert!(tier.share_bps > 0, "tier share must be positive");
            sum_bps += tier.share_bps;
        }

        assert!(sum_bps == 10_000, "shares must sum to exactly 10000 bps");

        for tier in tiers.iter() {
            let split_c = scale_commitment(total_commitment, tier.share_bps, 10_000);
            results.push_back(WaterfallResult {
                recipient: tier.recipient,
                commitment: split_c,
            });
        }

        results
    }

    /// Verify that the sum of the split parts equals the total commitment mod P.
    /// This proves mathematical correctness of the split without revealing any plaintext values.
    pub fn verify_split(_env: Env, parts: Vec<i128>, total: i128) -> bool {
        let mut sum: u128 = 0;
        for part in parts.iter() {
            sum = reduce_u128(sum + (part as u128));
        }
        (sum as i128) == total.rem_euclid(P)
    }

    /// Processes a repayment of `amount`, forwarding it to pool-manager as a
    /// reserve credit so LP NAV reflects the repaid capital. The
    /// cross-contract call panics (aborting this transaction, including any
    /// storage writes already made) if pool-manager rejects it — e.g. if the
    /// repayment would leave the pool with negative capital.
    pub fn process_repayment(env: Env, amount: i128) {
        assert!(amount > 0, "amount must be positive");

        let pool_manager: Address = env
            .storage()
            .instance()
            .get(&POOL_MANAGER)
            .unwrap_or_else(|| panic!("pool manager not configured"));

        PoolManagerClient::new(&env, &pool_manager).apply_reserve_delta(&amount);
    }

    /// Protocol ping — extend with domain logic.
    pub fn ping(env: Env, marker: Symbol) -> Symbol {
        let _ = env;
        marker
    }

    /// Contract ABI / deployment marker for integrators.
    pub fn version(_env: Env) -> u32 {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pool_manager::PoolManager;
    use soroban_sdk::symbol_short;

    fn setup() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();

        let pool_addr = env.register_contract(None::<&Address>, PoolManager);
        env.as_contract(&pool_addr, || {
            PoolManager::initialize(env.clone(), symbol_short!("admin"), 8_000);
            PoolManager::deposit(env.clone(), symbol_short!("alice"), 100_000);
        });

        let waterfall_addr = env.register_contract(None::<&Address>, RepaymentWaterfall);
        env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::initialize(env.clone(), symbol_short!("admin"), pool_addr.clone());
        });

        (env, waterfall_addr, pool_addr)
    }

    #[test]
    fn process_repayment_credits_pool_manager_capital() {
        let (env, waterfall_addr, pool_addr) = setup();

        env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::process_repayment(env.clone(), 5_000);
        });

        env.as_contract(&pool_addr, || {
            assert_eq!(PoolManager::total_capital(env.clone()), 105_000);
        });
    }

    #[test]
    #[should_panic(expected = "amount must be positive")]
    fn process_repayment_rejects_non_positive_amount() {
        let (env, waterfall_addr, _pool_addr) = setup();
        env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::process_repayment(env.clone(), 0);
        });
    }
}

// Contribution check by karen-s at 2024-11-28T20:49:39

// Contribution check by alexdev99 at 2025-03-05T02:20:41

// Contribution check by lisap at 2025-06-09T07:51:43

// Contribution check by karen-s at 2025-09-13T13:22:45

// Contribution check by alexdev99 at 2025-12-18T18:53:47

// Contribution check by lisap at 2026-03-25T00:24:49

// Contribution by CelestinaBeing — 2024-11-14

// Contribution by codemagician1949 — 2024-12-13

// Contribution by CelestinaBeing — 2025-01-11

// Contribution by codemagician1949 — 2025-02-10

// Contribution by CelestinaBeing — 2025-03-11

// Contribution by codemagician1949 — 2025-04-10

// Contribution by CelestinaBeing — 2025-05-09

// Contribution by codemagician1949 — 2025-06-07

// Contribution by CelestinaBeing — 2025-07-07

// Contribution by codemagician1949 — 2025-08-05

// Contribution by CelestinaBeing — 2025-09-04

// Contribution by codemagician1949 — 2025-10-03

// Contribution by CelestinaBeing — 2025-11-02

// Contribution by codemagician1949 — 2025-12-01

// Contribution by CelestinaBeing — 2025-12-30

// Contribution by codemagician1949 — 2026-01-29

// Contribution by CelestinaBeing — 2026-02-27

// Contribution by codemagician1949 — 2026-03-29

// Contribution by CelestinaBeing — 2026-04-27

// Contribution by codemagician1949 — 2026-05-26

#[cfg(test)]
mod split_tests {
    use super::*;
    use soroban_sdk::{symbol_short, Env};

    fn setup() -> Env {
        let env = Env::default();
        env.mock_all_auths();
        env
    }

    fn test_commitment(value: i128, blinding: i128) -> i128 {
        let vg = mod_mul(value, G, P);
        let rh = mod_mul(blinding, H, P);
        let sum = (vg as u128) + (rh as u128);
        reduce_u128(sum) as i128
    }

    #[test]
    fn test_split_commitment_two_tiers_sums_correctly() {
        let env = setup();
        let total_c = test_commitment(100_000, 1234);

        let mut tiers = Vec::new(&env);
        tiers.push_back(WaterfallTier {
            recipient: symbol_short!("lender"),
            share_bps: 8_000, // 80%
        });
        tiers.push_back(WaterfallTier {
            recipient: symbol_short!("fee"),
            share_bps: 2_000, // 20%
        });

        let results = RepaymentWaterfall::split_commitment(env.clone(), total_c, tiers);
        assert_eq!(results.len(), 2);

        let mut parts = Vec::new(&env);
        for res in results.iter() {
            parts.push_back(res.commitment);
        }

        assert!(RepaymentWaterfall::verify_split(
            env.clone(),
            parts,
            total_c
        ));
    }

    #[test]
    fn test_split_commitment_three_tiers_sums_correctly() {
        let env = setup();
        let total_c = test_commitment(250_000, 9999);

        let mut tiers = Vec::new(&env);
        tiers.push_back(WaterfallTier {
            recipient: symbol_short!("lender1"),
            share_bps: 5_000, // 50%
        });
        tiers.push_back(WaterfallTier {
            recipient: symbol_short!("lender2"),
            share_bps: 4_500, // 45%
        });
        tiers.push_back(WaterfallTier {
            recipient: symbol_short!("fee"),
            share_bps: 500, // 5%
        });

        let results = RepaymentWaterfall::split_commitment(env.clone(), total_c, tiers);
        assert_eq!(results.len(), 3);

        let mut parts = Vec::new(&env);
        for res in results.iter() {
            parts.push_back(res.commitment);
        }

        assert!(RepaymentWaterfall::verify_split(
            env.clone(),
            parts,
            total_c
        ));
    }

    #[test]
    #[should_panic(expected = "shares must sum to exactly 10000 bps")]
    fn test_split_invalid_bps_sum_panics() {
        let env = setup();
        let total_c = test_commitment(100_000, 1234);

        let mut tiers = Vec::new(&env);
        tiers.push_back(WaterfallTier {
            recipient: symbol_short!("lender"),
            share_bps: 8_000,
        });
        tiers.push_back(WaterfallTier {
            recipient: symbol_short!("fee"),
            share_bps: 1_999, // Sum is 9,999
        });

        let _ = RepaymentWaterfall::split_commitment(env, total_c, tiers);
    }

    #[test]
    #[should_panic(expected = "tier share must be positive")]
    fn test_split_zero_bps_panics() {
        let env = setup();
        let total_c = test_commitment(100_000, 1234);

        let mut tiers = Vec::new(&env);
        tiers.push_back(WaterfallTier {
            recipient: symbol_short!("lender"),
            share_bps: 10_000,
        });
        tiers.push_back(WaterfallTier {
            recipient: symbol_short!("fee"),
            share_bps: 0,
        });

        let _ = RepaymentWaterfall::split_commitment(env, total_c, tiers);
    }

    #[test]
    fn test_verify_split_tampered_fails() {
        let env = setup();
        let total_c = test_commitment(100_000, 1234);

        let mut parts = Vec::new(&env);
        // Sum to something else
        parts.push_back(test_commitment(50_000, 1234));
        parts.push_back(test_commitment(49_000, 1234));

        assert!(!RepaymentWaterfall::verify_split(env, parts, total_c));
    }
}
