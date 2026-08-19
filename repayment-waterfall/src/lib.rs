#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Env, Symbol, Vec,
};

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

/// Errors surfaced by the repayment waterfall. Stable `u32` discriminants so
/// integrators and audit tooling can match on them.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    /// `initialize` called on an already-initialized contract.
    AlreadyInitialized = 1,
    /// A repayment amount was not strictly positive.
    InvalidAmount = 2,
    /// `process_repayment` was called before `initialize` configured a pool manager.
    PoolManagerNotConfigured = 3,
    /// A tier's share was not strictly positive.
    InvalidTierShare = 4,
    /// Tier shares did not sum to exactly 10_000 basis points.
    InvalidBpsSum = 5,
    /// A tier share scale could not be inverted modulo P (not coprime).
    ScaleNotInvertible = 6,
}

/// Extended Euclidean algorithm for modular inverse.
fn mod_inverse(a: i128, m: i128) -> Result<i128, ContractError> {
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

    if old_r != 1 {
        return Err(ContractError::ScaleNotInvertible);
    }
    Ok(old_s.rem_euclid(m))
}

/// Scale a commitment mod P by a rational factor `scalar / scale`.
fn scale_commitment(c: i128, scalar: i128, scale: i128) -> Result<i128, ContractError> {
    let scaled = mod_mul(c, scalar, P);
    let inv = mod_inverse(scale, P)?;
    Ok(mod_mul(scaled, inv, P))
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

/// Tracks default volume accumulated within the current rolling 24h window
/// used by the circuit breaker (see `record_default`).
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultRecord {
    pub window_start: u64,
    pub volume: i128,
}

use soroban_sdk::{contractclient, Address};

const POOL_MANAGER: Symbol = symbol_short!("pool_mgr");

/// Storage key for the current rolling-window `DefaultRecord`.
const DEFAULT_RECORD: Symbol = symbol_short!("dflt_rec");
/// Storage key for the configured circuit-breaker default-volume threshold.
const CB_THRESHOLD: Symbol = symbol_short!("cb_thresh");
/// Storage key for the circuit-breaker tripped flag.
const CB_ACTIVE: Symbol = symbol_short!("cb_active");

/// Length of the rolling default-volume window, in seconds (24h), mirroring
/// the `env.ledger().timestamp()` idiom used for pool-manager's timelock.
const CIRCUIT_BREAKER_WINDOW_SECS: u64 = 86_400;

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
    pub fn initialize(env: Env, admin: Symbol, pool_manager: Address) -> Result<(), ContractError> {
        if env.storage().instance().has(&symbol_short!("admin")) {
            return Err(ContractError::AlreadyInitialized);
        }
        env.storage()
            .instance()
            .set(&symbol_short!("admin"), &admin);
        env.storage().instance().set(&POOL_MANAGER, &pool_manager);
        Ok(())
    }

    /// Split a total confidential commitment across multiple tiers according to basis points.
    /// The sum of the split commitments is guaranteed to match the total commitment homomorphically.
    pub fn split_commitment(
        env: Env,
        total_commitment: i128,
        tiers: Vec<WaterfallTier>,
    ) -> Result<Vec<WaterfallResult>, ContractError> {
        Self::require_not_paused(&env);

        let mut results = Vec::new(&env);
        let mut sum_bps: i128 = 0;

        for tier in tiers.iter() {
            if tier.share_bps <= 0 {
                return Err(ContractError::InvalidTierShare);
            }
            sum_bps += tier.share_bps;
        }

        if sum_bps != 10_000 {
            return Err(ContractError::InvalidBpsSum);
        }

        for tier in tiers.iter() {
            let split_c = scale_commitment(total_commitment, tier.share_bps, 10_000)?;
            results.push_back(WaterfallResult {
                recipient: tier.recipient,
                commitment: split_c,
            });
        }

        Ok(results)
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
    pub fn process_repayment(env: Env, amount: i128) -> Result<(), ContractError> {
        Self::require_not_paused(&env);
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let pool_manager: Address = env
            .storage()
            .instance()
            .get(&POOL_MANAGER)
            .ok_or(ContractError::PoolManagerNotConfigured)?;

        PoolManagerClient::new(&env, &pool_manager).apply_reserve_delta(&amount);
        Ok(())
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

    // ── circuit breaker: default surge protection ──────────────────────

    /// Records a default of `amount` against the rolling 24h default-volume
    /// window. Integration point: this is intended to be invoked whenever a
    /// default occurs — plausibly by `pool-manager` (once it tracks default
    /// state) or by an off-chain monitoring/liquidation job — so the
    /// waterfall can react to a surge in defaults independent of any single
    /// invoice's own lifecycle. Resets the window (`window_start` = now,
    /// `volume` = `amount`) if 24h (`CIRCUIT_BREAKER_WINDOW_SECS`) have
    /// elapsed since the window began, otherwise accumulates `amount` into
    /// the existing window's `volume`. If the resulting volume meets or
    /// exceeds the configured threshold, trips the circuit breaker: sets the
    /// `CircuitBreakerActive` flag and emits `CircuitBreakerTripped`
    /// (topic `cb_trip`, data `(volume, threshold)`).
    pub fn record_default(env: Env, amount: i128) {
        assert!(amount > 0, "amount must be positive");

        let now = env.ledger().timestamp();
        let mut record: DefaultRecord =
            env.storage()
                .instance()
                .get(&DEFAULT_RECORD)
                .unwrap_or(DefaultRecord {
                    window_start: now,
                    volume: 0,
                });

        if now.saturating_sub(record.window_start) >= CIRCUIT_BREAKER_WINDOW_SECS {
            record.window_start = now;
            record.volume = amount;
        } else {
            record.volume += amount;
        }

        env.storage().instance().set(&DEFAULT_RECORD, &record);

        let threshold: i128 = env.storage().instance().get(&CB_THRESHOLD).unwrap_or(0);

        if threshold > 0 && record.volume >= threshold {
            env.storage().instance().set(&CB_ACTIVE, &true);
            env.events()
                .publish((symbol_short!("cb_trip"),), (record.volume, threshold));
        }
    }

    /// Admin-gated. Sets the rolling 24h default-volume threshold above
    /// which `record_default` trips the circuit breaker.
    pub fn set_circuit_breaker_threshold(env: Env, admin: Symbol, threshold: i128) {
        Self::require_admin(&env, &admin);
        assert!(threshold > 0, "threshold must be positive");
        env.storage().instance().set(&CB_THRESHOLD, &threshold);
    }

    /// Admin-gated. Resumes processing after a circuit breaker trip has been
    /// reviewed, clearing `CircuitBreakerActive` and resetting the rolling
    /// default-volume window.
    pub fn resume_processing(env: Env, admin: Symbol) {
        Self::require_admin(&env, &admin);
        env.storage().instance().set(&CB_ACTIVE, &false);
        env.storage().instance().set(
            &DEFAULT_RECORD,
            &DefaultRecord {
                window_start: env.ledger().timestamp(),
                volume: 0,
            },
        );
    }

    /// Whether the circuit breaker is currently tripped.
    pub fn circuit_breaker_active(env: Env) -> bool {
        env.storage().instance().get(&CB_ACTIVE).unwrap_or(false)
    }

    /// The default volume accumulated in the current rolling 24h window.
    pub fn current_default_volume(env: Env) -> i128 {
        env.storage()
            .instance()
            .get::<Symbol, DefaultRecord>(&DEFAULT_RECORD)
            .map(|r| r.volume)
            .unwrap_or(0)
    }

    /// Panics if the circuit breaker is currently tripped. Called at the top
    /// of every repayment-processing entry point (`split_commitment`,
    /// `process_repayment`) so a trip pauses all repayment-waterfall
    /// processing.
    fn require_not_paused(env: &Env) {
        let active: bool = env.storage().instance().get(&CB_ACTIVE).unwrap_or(false);
        assert!(!active, "circuit breaker active: processing paused");
    }

    fn require_admin(env: &Env, caller: &Symbol) {
        let admin: Symbol = env
            .storage()
            .instance()
            .get(&symbol_short!("admin"))
            .expect("not initialized");
        assert!(*caller == admin, "only admin");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pool_manager::PoolManager;
    use soroban_sdk::symbol_short;
    use soroban_sdk::testutils::Ledger;

    fn setup() -> (Env, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();

        let pool_addr = env.register_contract(None::<&Address>, PoolManager);
        env.as_contract(&pool_addr, || {
            PoolManager::initialize(env.clone(), symbol_short!("admin"), 8_000).unwrap();
            PoolManager::deposit(env.clone(), symbol_short!("alice"), 100_000).unwrap();
        });

        let waterfall_addr = env.register_contract(None::<&Address>, RepaymentWaterfall);
        env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::initialize(env.clone(), symbol_short!("admin"), pool_addr.clone())
                .unwrap();
        });

        (env, waterfall_addr, pool_addr)
    }

    #[test]
    fn process_repayment_credits_pool_manager_capital() {
        let (env, waterfall_addr, pool_addr) = setup();

        env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::process_repayment(env.clone(), 5_000).unwrap();
        });

        env.as_contract(&pool_addr, || {
            assert_eq!(PoolManager::total_capital(env.clone()), 105_000);
        });
    }

    #[test]
    fn process_repayment_rejects_non_positive_amount() {
        let (env, waterfall_addr, _pool_addr) = setup();
        let err = env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::process_repayment(env.clone(), 0)
        });
        assert_eq!(err, Err(ContractError::InvalidAmount));
    }

    // ── circuit breaker: default surge protection ──────────────────────

    #[test]
    fn record_default_accumulates_within_window() {
        let (env, waterfall_addr, _pool_addr) = setup();

        env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::record_default(env.clone(), 1_000);
            RepaymentWaterfall::record_default(env.clone(), 2_000);
            assert_eq!(RepaymentWaterfall::current_default_volume(env.clone()), 3_000);
            assert!(!RepaymentWaterfall::circuit_breaker_active(env.clone()));
        });
    }

    #[test]
    fn record_default_resets_after_24h() {
        let (env, waterfall_addr, _pool_addr) = setup();

        env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::record_default(env.clone(), 1_000);
        });

        env.ledger().with_mut(|li| li.timestamp += 86_400);

        env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::record_default(env.clone(), 500);
            assert_eq!(RepaymentWaterfall::current_default_volume(env.clone()), 500);
        });
    }

    #[test]
    fn circuit_breaker_trips_at_threshold() {
        let (env, waterfall_addr, _pool_addr) = setup();

        env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::set_circuit_breaker_threshold(
                env.clone(),
                symbol_short!("admin"),
                5_000,
            );
            assert!(!RepaymentWaterfall::circuit_breaker_active(env.clone()));

            RepaymentWaterfall::record_default(env.clone(), 5_000);
            assert!(RepaymentWaterfall::circuit_breaker_active(env.clone()));
        });
    }

    #[test]
    #[should_panic(expected = "circuit breaker active: processing paused")]
    fn processing_blocked_while_circuit_breaker_tripped() {
        let (env, waterfall_addr, _pool_addr) = setup();

        env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::set_circuit_breaker_threshold(
                env.clone(),
                symbol_short!("admin"),
                1_000,
            );
            RepaymentWaterfall::record_default(env.clone(), 1_000);
            let _ = RepaymentWaterfall::process_repayment(env.clone(), 100);
        });
    }

    #[test]
    fn admin_can_resume_processing_after_trip() {
        let (env, waterfall_addr, pool_addr) = setup();

        env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::set_circuit_breaker_threshold(
                env.clone(),
                symbol_short!("admin"),
                1_000,
            );
            RepaymentWaterfall::record_default(env.clone(), 1_000);
            assert!(RepaymentWaterfall::circuit_breaker_active(env.clone()));

            RepaymentWaterfall::resume_processing(env.clone(), symbol_short!("admin"));
            assert!(!RepaymentWaterfall::circuit_breaker_active(env.clone()));
            assert_eq!(RepaymentWaterfall::current_default_volume(env.clone()), 0);

            RepaymentWaterfall::process_repayment(env.clone(), 100).unwrap();
        });

        env.as_contract(&pool_addr, || {
            assert_eq!(PoolManager::total_capital(env.clone()), 100_100);
        });
    }

    #[test]
    #[should_panic(expected = "only admin")]
    fn non_admin_resume_is_rejected() {
        let (env, waterfall_addr, _pool_addr) = setup();

        env.as_contract(&waterfall_addr, || {
            RepaymentWaterfall::set_circuit_breaker_threshold(
                env.clone(),
                symbol_short!("admin"),
                1_000,
            );
            RepaymentWaterfall::record_default(env.clone(), 1_000);
            RepaymentWaterfall::resume_processing(env.clone(), symbol_short!("not_admin"));
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

    fn setup() -> (Env, Address) {
        let env = Env::default();
        env.mock_all_auths();
        // A registered (but uninitialized) contract instance so calls that
        // touch instance storage (e.g. `require_not_paused`'s circuit-breaker
        // check) have a real invocation context to read from, matching how
        // Soroban actually invokes contracts in production.
        let addr = env.register_contract(None::<&Address>, RepaymentWaterfall);
        (env, addr)
    }

    fn test_commitment(value: i128, blinding: i128) -> i128 {
        let vg = mod_mul(value, G, P);
        let rh = mod_mul(blinding, H, P);
        let sum = (vg as u128) + (rh as u128);
        reduce_u128(sum) as i128
    }

    #[test]
    fn test_split_commitment_two_tiers_sums_correctly() {
        let (env, addr) = setup();
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

        let results = env
            .as_contract(&addr, || {
                RepaymentWaterfall::split_commitment(env.clone(), total_c, tiers)
            })
            .unwrap();
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
        let (env, addr) = setup();
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

        let results = env
            .as_contract(&addr, || {
                RepaymentWaterfall::split_commitment(env.clone(), total_c, tiers)
            })
            .unwrap();
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
    fn test_split_invalid_bps_sum_errors() {
        let (env, addr) = setup();
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

        let result = env.as_contract(&addr, || {
            RepaymentWaterfall::split_commitment(env.clone(), total_c, tiers)
        });
        assert_eq!(result, Err(ContractError::InvalidBpsSum));
    }

    #[test]
    fn test_split_zero_bps_errors() {
        let (env, addr) = setup();
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

        let result = env.as_contract(&addr, || {
            RepaymentWaterfall::split_commitment(env.clone(), total_c, tiers)
        });
        assert_eq!(result, Err(ContractError::InvalidTierShare));
    }

    #[test]
    fn test_verify_split_tampered_fails() {
        let (env, _addr) = setup();
        let total_c = test_commitment(100_000, 1234);

        let mut parts = Vec::new(&env);
        // Sum to something else
        parts.push_back(test_commitment(50_000, 1234));
        parts.push_back(test_commitment(49_000, 1234));

        assert!(!RepaymentWaterfall::verify_split(env, parts, total_c));
    }
}
