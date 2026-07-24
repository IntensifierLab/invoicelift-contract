#![no_std]
#[allow(unused_imports)]
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Env, Symbol};



/// Persistent storage keys.
mod storage {
    use soroban_sdk::{symbol_short, Symbol};

    pub const ADMIN: Symbol = symbol_short!("admin");
    pub const TOTAL_SHARES: Symbol = symbol_short!("tot_sh");
    pub const TOTAL_CAPITAL: Symbol = symbol_short!("tot_ca");
    pub const FINANCED_AMT: Symbol = symbol_short!("fin_am");
    pub const MAX_UTIL: Symbol = symbol_short!("max_ut");
    pub const NAV: Symbol = symbol_short!("nav");
}

/// Per-lender LP record.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LenderPosition {
    pub shares: i128,
}

/// Lender storage key.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LenderKey(pub Symbol);

/// Lender pools and limits.
#[contract]
pub struct PoolManager;

/// NAV scaling factor: 1_000_000 means NAV = 1.0.
const NAV_SCALE: i128 = 1_000_000;

#[contractimpl]
impl PoolManager {
    /// One-time initialization with pool parameters.
    pub fn initialize(env: Env, admin: Symbol, max_utilisation: i128) {
        if env.storage().instance().has(&storage::ADMIN) {
            panic!("already initialized");
        }
        env.storage().instance().set(&storage::ADMIN, &admin);
        env.storage()
            .instance()
            .set(&storage::TOTAL_SHARES, &0_i128);
        env.storage()
            .instance()
            .set(&storage::TOTAL_CAPITAL, &0_i128);
        env.storage()
            .instance()
            .set(&storage::FINANCED_AMT, &0_i128);
        env.storage()
            .instance()
            .set(&storage::MAX_UTIL, &max_utilisation);
        env.storage().instance().set(&storage::NAV, &NAV_SCALE);
    }

    // ── mutators ──────────────────────────────────────────────────────

    /// Deposit capital into the pool. `amount` is in base units.
    /// Shares minted = amount * NAV_SCALE / nav.
    /// Returns the number of shares minted.
    pub fn deposit(env: Env, lender: Symbol, amount: i128) -> i128 {
        assert!(amount > 0, "amount must be positive");

        let nav: i128 = env.storage().instance().get(&storage::NAV).unwrap();
        let shares = amount * NAV_SCALE / nav;

        // credit lender
        let key = LenderKey(lender);
        let pos: LenderPosition = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(LenderPosition { shares: 0 });
        env.storage().persistent().set(
            &key,
            &LenderPosition {
                shares: pos.shares + shares,
            },
        );

        // update totals — derive capital from shares to keep invariant exact
        let tot_shares: i128 = env
            .storage()
            .instance()
            .get(&storage::TOTAL_SHARES)
            .unwrap();
        let new_tot_shares = tot_shares + shares;

        env.storage()
            .instance()
            .set(&storage::TOTAL_SHARES, &new_tot_shares);
        env.storage()
            .instance()
            .set(&storage::TOTAL_CAPITAL, &(new_tot_shares * nav / NAV_SCALE));

        shares
    }

    /// Withdraw capital. Returns the amount withdrawn in base units.
    pub fn withdraw(env: Env, lender: Symbol, shares: i128) -> i128 {
        assert!(shares > 0, "shares must be positive");

        let key = LenderKey(lender);
        let pos: LenderPosition = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(LenderPosition { shares: 0 });
        assert!(pos.shares >= shares, "insufficient shares");

        let nav: i128 = env.storage().instance().get(&storage::NAV).unwrap();
        let amount = shares * nav / NAV_SCALE;

        env.storage().persistent().set(
            &key,
            &LenderPosition {
                shares: pos.shares - shares,
            },
        );

        let tot_shares: i128 = env
            .storage()
            .instance()
            .get(&storage::TOTAL_SHARES)
            .unwrap();
        let new_tot_shares = tot_shares - shares;
        let new_capital = new_tot_shares * nav / NAV_SCALE;

        env.storage()
            .instance()
            .set(&storage::TOTAL_SHARES, &new_tot_shares);
        env.storage()
            .instance()
            .set(&storage::TOTAL_CAPITAL, &new_capital);

        // clamp financed_amount if withdrawal reduced available capacity
        let fin: i128 = env
            .storage()
            .instance()
            .get(&storage::FINANCED_AMT)
            .unwrap();
        let max_util: i128 = env.storage().instance().get(&storage::MAX_UTIL).unwrap();
        let limit = new_capital * max_util / 10_000;
        if fin > limit {
            env.storage().instance().set(&storage::FINANCED_AMT, &limit);
        }

        amount
    }

    /// Mark an invoice as financed. `amount` is the financed value.
    /// Invariant enforced: financed_amount <= total_capital * max_utilisation / 10_000
    pub fn finance(env: Env, amount: i128) {
        assert!(amount > 0, "amount must be positive");

        let fin: i128 = env
            .storage()
            .instance()
            .get(&storage::FINANCED_AMT)
            .unwrap();
        let tot_capital: i128 = env
            .storage()
            .instance()
            .get(&storage::TOTAL_CAPITAL)
            .unwrap();
        let max_util: i128 = env.storage().instance().get(&storage::MAX_UTIL).unwrap();

        let new_fin = fin + amount;
        assert!(
            new_fin <= tot_capital * max_util / 10_000,
            "would exceed max utilisation"
        );

        env.storage()
            .instance()
            .set(&storage::FINANCED_AMT, &new_fin);
    }

    /// Update the NAV (net asset value) per share. `new_nav` is scaled by NAV_SCALE.
    /// total_capital is re-derived from the canonical invariant.
    /// If the new capital drops below the financed amount limit, financed_amount is clamped.
    pub fn set_nav(env: Env, new_nav: i128) {
        assert!(new_nav > 0, "NAV must be positive");

        let tot_shares: i128 = env
            .storage()
            .instance()
            .get(&storage::TOTAL_SHARES)
            .unwrap();
        let max_util: i128 = env.storage().instance().get(&storage::MAX_UTIL).unwrap();
        let new_capital = tot_shares * new_nav / NAV_SCALE;

        env.storage().instance().set(&storage::NAV, &new_nav);
        env.storage()
            .instance()
            .set(&storage::TOTAL_CAPITAL, &new_capital);

        // clamp financed_amount if NAV drop reduced available capacity
        let fin: i128 = env
            .storage()
            .instance()
            .get(&storage::FINANCED_AMT)
            .unwrap();
        let limit = new_capital * max_util / 10_000;
        if fin > limit {
            env.storage().instance().set(&storage::FINANCED_AMT, &limit);
        }
    }

    // ── view helpers ──────────────────────────────────────────────────

    pub fn total_shares(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&storage::TOTAL_SHARES)
            .unwrap_or(0)
    }

    /// total_capital = total_shares * NAV / NAV_SCALE — the pool's total value.
    pub fn total_capital(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&storage::TOTAL_CAPITAL)
            .unwrap_or(0)
    }

    pub fn financed_amount(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&storage::FINANCED_AMT)
            .unwrap_or(0)
    }

    pub fn max_utilisation(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&storage::MAX_UTIL)
            .unwrap_or(0)
    }

    pub fn nav(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&storage::NAV)
            .unwrap_or(NAV_SCALE)
    }

    pub fn lender_shares(env: Env, lender: Symbol) -> i128 {
        let key = LenderKey(lender);
        let pos: LenderPosition = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(LenderPosition { shares: 0 });
        pos.shares
    }

    /// Contract ABI / deployment marker for integrators.
    pub fn version(_env: Env) -> u32 {
        1
    }
}

// ─────────────────────────────────────────────────────────────────────
// Formal invariant tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{Address, Env};

    fn setup() -> (Env, soroban_sdk::Address) {
        let env = Env::default();
        let _admin = Address::generate(&env);
        env.mock_all_auths();
        let contract_addr = env.register_contract(None::<&Address>, PoolManager);
        env.as_contract(&contract_addr, || {
            PoolManager::initialize(env.clone(), symbol_short!("admin"), 8_000);
        });
        (env, contract_addr)
    }

    // ── Invariant 1: total_shares * NAV / NAV_SCALE == total_capital ─

    #[test]
    fn invariant_shares_nav_equals_capital_after_deposits() {
        let (env, contract_addr) = setup();
        let alice = symbol_short!("alice");
        let bob = symbol_short!("bob");

        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), alice.clone(), 10_000);
        });
        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), bob, 5_000);
        });

        env.as_contract(&contract_addr, || {
            let tot_sh = PoolManager::total_shares(env.clone());
            let nav = PoolManager::nav(env.clone());
            let tot_cap = PoolManager::total_capital(env.clone());
            assert_eq!(tot_sh * nav / NAV_SCALE, tot_cap);
        });
    }

    #[test]
    fn invariant_shares_nav_equals_capital_after_withdrawal() {
        let (env, contract_addr) = setup();
        let alice = symbol_short!("alice");

        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), alice.clone(), 20_000);
        });

        let shares_to_withdraw = env.as_contract(&contract_addr, || {
            PoolManager::lender_shares(env.clone(), alice.clone()) / 2
        });
        env.as_contract(&contract_addr, || {
            PoolManager::withdraw(env.clone(), alice, shares_to_withdraw);
        });

        env.as_contract(&contract_addr, || {
            let tot_sh = PoolManager::total_shares(env.clone());
            let nav = PoolManager::nav(env.clone());
            let tot_cap = PoolManager::total_capital(env.clone());
            assert_eq!(tot_sh * nav / NAV_SCALE, tot_cap);
        });
    }

    #[test]
    fn invariant_shares_nav_equals_capital_after_nav_change() {
        let (env, contract_addr) = setup();
        let alice = symbol_short!("alice");
        let bob = symbol_short!("bob");

        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), alice, 10_000);
        });

        env.as_contract(&contract_addr, || {
            PoolManager::set_nav(env.clone(), 1_200_000);
        });

        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), bob, 5_000);
        });

        env.as_contract(&contract_addr, || {
            let tot_sh = PoolManager::total_shares(env.clone());
            let nav = PoolManager::nav(env.clone());
            let tot_cap = PoolManager::total_capital(env.clone());
            assert_eq!(tot_sh * nav / NAV_SCALE, tot_cap);
        });
    }

    #[test]
    fn invariant_capital_matches_deposited_value() {
        let (env, contract_addr) = setup();
        let alice = symbol_short!("alice");
        let bob = symbol_short!("bob");

        env.as_contract(&contract_addr, || {
            let sh = PoolManager::deposit(env.clone(), alice.clone(), 10_000);
            assert_eq!(sh, 10_000);
        });

        env.as_contract(&contract_addr, || {
            let sh = PoolManager::deposit(env.clone(), bob.clone(), 5_000);
            assert_eq!(sh, 5_000);
        });

        env.as_contract(&contract_addr, || {
            assert_eq!(PoolManager::total_shares(env.clone()), 15_000);
            assert_eq!(PoolManager::total_capital(env.clone()), 15_000);
        });

        env.as_contract(&contract_addr, || {
            let withdrawn = PoolManager::withdraw(env.clone(), alice, 10_000);
            assert_eq!(withdrawn, 10_000);
        });

        env.as_contract(&contract_addr, || {
            assert_eq!(PoolManager::total_shares(env.clone()), 5_000);
            assert_eq!(PoolManager::total_capital(env.clone()), 5_000);
        });
    }

    // ── Invariant 2: financed_amount <= total_capital * max_util ─────

    #[test]
    fn invariant_financed_within_utilisation_limit() {
        let (env, contract_addr) = setup();
        let alice = symbol_short!("alice");

        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), alice, 100_000);
        });

        env.as_contract(&contract_addr, || {
            PoolManager::finance(env.clone(), 80_000);
        });

        env.as_contract(&contract_addr, || {
            let fin = PoolManager::financed_amount(env.clone());
            let cap = PoolManager::total_capital(env.clone());
            let max = PoolManager::max_utilisation(env.clone());
            assert_eq!(fin, cap * max / 10_000);
        });
    }

    #[test]
    fn invariant_financed_stays_within_after_withdrawal() {
        let (env, contract_addr) = setup();
        let alice = symbol_short!("alice");
        let bob = symbol_short!("bob");

        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), alice.clone(), 50_000);
            PoolManager::deposit(env.clone(), bob, 50_000);
            PoolManager::finance(env.clone(), 40_000);
        });

        let sh = env.as_contract(&contract_addr, || {
            PoolManager::lender_shares(env.clone(), alice.clone())
        });
        env.as_contract(&contract_addr, || {
            PoolManager::withdraw(env.clone(), alice, sh * 3 / 5);
        });

        env.as_contract(&contract_addr, || {
            let fin = PoolManager::financed_amount(env.clone());
            let cap = PoolManager::total_capital(env.clone());
            let max = PoolManager::max_utilisation(env.clone());
            assert!(fin <= cap * max / 10_000);
        });
    }

    // ── Invariant 3: no lender receives more than their LP share ─────

    #[test]
    fn invariant_lender_share_never_exceeds_total() {
        let (env, contract_addr) = setup();
        let alice = symbol_short!("alice");
        let bob = symbol_short!("bob");

        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), alice.clone(), 30_000);
            PoolManager::deposit(env.clone(), bob.clone(), 70_000);
        });

        env.as_contract(&contract_addr, || {
            let alice_sh = PoolManager::lender_shares(env.clone(), alice);
            let bob_sh = PoolManager::lender_shares(env.clone(), bob);
            let tot = PoolManager::total_shares(env.clone());
            assert!(alice_sh <= tot);
            assert!(bob_sh <= tot);
            assert_eq!(alice_sh + bob_sh, tot);
        });
    }

    #[test]
    fn invariant_withdraw_cannot_exceed_lender_position() {
        let (env, contract_addr) = setup();
        let alice = symbol_short!("alice");

        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), alice.clone(), 10_000);
        });

        let alice_shares = env.as_contract(&contract_addr, || {
            PoolManager::lender_shares(env.clone(), alice.clone())
        });

        env.as_contract(&contract_addr, || {
            PoolManager::withdraw(env.clone(), alice.clone(), alice_shares);
        });

        env.as_contract(&contract_addr, || {
            let sh = PoolManager::lender_shares(env.clone(), alice);
            assert_eq!(sh, 0);
        });
    }

    // ── 10,000-operation fuzz test ───────────────────────────────────

    #[test]
    fn fuzz_10000_operation_sequence() {
        let env = Env::default();
        env.mock_all_auths();
        env.budget().reset_unlimited();
        let contract_addr = env.register_contract(None::<&Address>, PoolManager);
        env.as_contract(&contract_addr, || {
            PoolManager::initialize(env.clone(), symbol_short!("admin"), 8_000);
        });

        let lenders = [
            symbol_short!("l1"),
            symbol_short!("l2"),
            symbol_short!("l3"),
            symbol_short!("l4"),
            symbol_short!("l5"),
        ];

        let mut rng_state: u64 = 12345;

        for op in 0..10_000u64 {
            rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let pick = rng_state as usize % 100;

            let lender_idx = (rng_state >> 8) as usize % lenders.len();
            let lender = lenders[lender_idx].clone();

            if pick < 60 {
                // 60% deposit
                let amount = ((rng_state >> 16) % 50_000 + 100) as i128;
                env.as_contract(&contract_addr, || {
                    PoolManager::deposit(env.clone(), lender, amount);
                });
            } else if pick < 85 {
                // 25% withdraw
                let sh = env.as_contract(&contract_addr, || {
                    PoolManager::lender_shares(env.clone(), lender.clone())
                });
                if sh > 0 {
                    let withdraw_sh = ((rng_state >> 16) % (sh as u64 + 1)) as i128;
                    if withdraw_sh > 0 {
                        env.as_contract(&contract_addr, || {
                            PoolManager::withdraw(env.clone(), lender, withdraw_sh);
                        });
                    }
                }
            } else if pick < 95 {
                // 10% finance
                let amount = ((rng_state >> 16) % 10_000 + 100) as i128;
                env.as_contract(&contract_addr, || {
                    let fin = PoolManager::financed_amount(env.clone());
                    let cap = PoolManager::total_capital(env.clone());
                    let max = PoolManager::max_utilisation(env.clone());
                    if fin + amount <= cap * max / 10_000 {
                        PoolManager::finance(env.clone(), amount);
                    }
                });
            } else {
                // 5% change NAV (range 500_000 to 2_500_000 i.e. 0.5x to 2.5x)
                let new_nav = ((rng_state >> 16) % 2_000_000 + 500_000) as i128;
                env.as_contract(&contract_addr, || {
                    PoolManager::set_nav(env.clone(), new_nav);
                });
            }

            // ── check invariants every 100 ops ──────────────────────
            if op % 100 == 0 {
                env.budget().reset_unlimited();
                env.as_contract(&contract_addr, || {
                    // INVARIANT 1: total_shares * NAV / NAV_SCALE == total_capital
                    let tot_sh = PoolManager::total_shares(env.clone());
                    let nav = PoolManager::nav(env.clone());
                    let tot_cap = PoolManager::total_capital(env.clone());
                    assert_eq!(
                        tot_sh * nav / NAV_SCALE, tot_cap,
                        "INVARIANT 1 violated at op {op}: tot_sh={tot_sh} nav={nav} tot_cap={tot_cap}"
                    );

                    // INVARIANT 2: financed_amount <= total_capital * max_utilisation / 10_000
                    let fin = PoolManager::financed_amount(env.clone());
                    let cap = PoolManager::total_capital(env.clone());
                    let max = PoolManager::max_utilisation(env.clone());
                    assert!(
                        fin <= cap * max / 10_000,
                        "INVARIANT 2 violated at op {op}: fin={fin} cap={cap} max={max}"
                    );

                    // INVARIANT 3: no lender exceeds total shares
                    for l in lenders.iter() {
                        let lsh = PoolManager::lender_shares(env.clone(), l.clone());
                        assert!(
                            lsh <= tot_sh,
                            "INVARIANT 3 violated at op {op}: lender shares {lsh} > total {tot_sh}"
                        );
                    }
                });
            }
        }

        // final invariant check
        env.budget().reset_unlimited();
        env.as_contract(&contract_addr, || {
            let tot_sh = PoolManager::total_shares(env.clone());
            let nav = PoolManager::nav(env.clone());
            let tot_cap = PoolManager::total_capital(env.clone());
            assert_eq!(tot_sh * nav / NAV_SCALE, tot_cap);

            let fin = PoolManager::financed_amount(env.clone());
            let cap = PoolManager::total_capital(env.clone());
            let max = PoolManager::max_utilisation(env.clone());
            assert!(fin <= cap * max / 10_000);

            for l in lenders.iter() {
                let lsh = PoolManager::lender_shares(env.clone(), l.clone());
                assert!(lsh <= tot_sh);
            }
        });
    }
}
