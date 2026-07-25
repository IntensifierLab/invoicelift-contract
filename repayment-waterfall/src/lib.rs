#![no_std]
use soroban_sdk::{contract, contractclient, contractimpl, symbol_short, Address, Env, Symbol};

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
        env.storage().instance().set(&symbol_short!("admin"), &admin);
        env.storage().instance().set(&POOL_MANAGER, &pool_manager);
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
        1
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
