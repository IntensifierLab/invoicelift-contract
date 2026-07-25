#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Env, Symbol, Vec};

/// Persistent storage keys.
mod storage {
    use soroban_sdk::{symbol_short, Symbol};

    pub const ADMIN: Symbol = symbol_short!("admin");
    pub const TOTAL_SHARES: Symbol = symbol_short!("tot_sh");
    pub const TOTAL_CAPITAL: Symbol = symbol_short!("tot_ca");
    pub const FINANCED_AMT: Symbol = symbol_short!("fin_am");
    pub const MAX_UTIL: Symbol = symbol_short!("max_ut");
    pub const NAV: Symbol = symbol_short!("nav");
    /// Governance flag: excludes this pool from being picked as a rebalancing donor.
    pub const DONOR_BLK: Symbol = symbol_short!("dnr_blk");
    /// Address authorized to queue/cancel timelocked admin actions. Separate
    /// from the legacy `ADMIN` symbol tag: this is a real `Address` so it can
    /// be authenticated with `require_auth`.
    pub const ADMIN_ADDR: Symbol = symbol_short!("adm_addr");
    /// Next id to assign to a queued timelocked action.
    pub const NEXT_ACTION: Symbol = symbol_short!("nxt_act");
}

/// Reserve coverage floor, in basis points (500 = 5%). Rebalancing targets keeping
/// every pool's reserve (capital not currently financed) at or above this floor.
const RESERVE_FLOOR_BPS: i128 = 500;
/// A donation in a single rebalance is capped at this fraction of the donor's
/// excess reserve above its own floor (5_000 = 50%).
const DONOR_CAP_BPS: i128 = 5_000;
const BPS_SCALE: i128 = 10_000;

/// Minimum delay between queuing an admin parameter change and executing it,
/// giving LPs an exit window before the change takes effect.
const TIMELOCK_SECS: u64 = 48 * 60 * 60;

/// A queued, timelocked change to a named admin parameter.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedAction {
    pub id: u32,
    pub param: Symbol,
    pub new_value: i128,
    pub queued_at: u64,
    pub execute_after: u64,
    pub executed: bool,
    pub cancelled: bool,
}

/// Queued-action storage key.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionKey(pub u32);

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
    pub fn initialize(
        env: Env,
        admin: Symbol,
        max_utilisation: i128,
    ) {
        if env.storage().instance().has(&storage::ADMIN) {
            panic!("already initialized");
        }
        env.storage().instance().set(&storage::ADMIN, &admin);
        env.storage().instance().set(&storage::TOTAL_SHARES, &0_i128);
        env.storage().instance().set(&storage::TOTAL_CAPITAL, &0_i128);
        env.storage().instance().set(&storage::FINANCED_AMT, &0_i128);
        env.storage().instance().set(&storage::MAX_UTIL, &max_utilisation);
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
        env.storage()
            .persistent()
            .set(&key, &LenderPosition { shares: pos.shares + shares });

        // update totals — derive capital from shares to keep invariant exact
        let tot_shares: i128 = env.storage().instance().get(&storage::TOTAL_SHARES).unwrap();
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

        env.storage()
            .persistent()
            .set(&key, &LenderPosition { shares: pos.shares - shares });

        let tot_shares: i128 = env.storage().instance().get(&storage::TOTAL_SHARES).unwrap();
        let new_tot_shares = tot_shares - shares;
        let new_capital = new_tot_shares * nav / NAV_SCALE;

        env.storage()
            .instance()
            .set(&storage::TOTAL_SHARES, &new_tot_shares);
        env.storage()
            .instance()
            .set(&storage::TOTAL_CAPITAL, &new_capital);

        // clamp financed_amount if withdrawal reduced available capacity
        let fin: i128 = env.storage().instance().get(&storage::FINANCED_AMT).unwrap();
        let max_util: i128 = env.storage().instance().get(&storage::MAX_UTIL).unwrap();
        let limit = new_capital * max_util / 10_000;
        if fin > limit {
            env.storage()
                .instance()
                .set(&storage::FINANCED_AMT, &limit);
        }

        amount
    }

    /// Mark an invoice as financed. `amount` is the financed value.
    /// Invariant enforced: financed_amount <= total_capital * max_utilisation / 10_000
    pub fn finance(env: Env, amount: i128) {
        assert!(amount > 0, "amount must be positive");

        let fin: i128 = env.storage().instance().get(&storage::FINANCED_AMT).unwrap();
        let tot_capital: i128 = env.storage().instance().get(&storage::TOTAL_CAPITAL).unwrap();
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

        let tot_shares: i128 = env.storage().instance().get(&storage::TOTAL_SHARES).unwrap();
        let max_util: i128 = env.storage().instance().get(&storage::MAX_UTIL).unwrap();
        let new_capital = tot_shares * new_nav / NAV_SCALE;

        env.storage().instance().set(&storage::NAV, &new_nav);
        env.storage()
            .instance()
            .set(&storage::TOTAL_CAPITAL, &new_capital);

        // clamp financed_amount if NAV drop reduced available capacity
        let fin: i128 = env.storage().instance().get(&storage::FINANCED_AMT).unwrap();
        let limit = new_capital * max_util / 10_000;
        if fin > limit {
            env.storage()
                .instance()
                .set(&storage::FINANCED_AMT, &limit);
        }
    }

    // ── view helpers ──────────────────────────────────────────────────

    pub fn total_shares(env: Env) -> i128 {
        env.storage().instance().get(&storage::TOTAL_SHARES).unwrap_or(0)
    }

    /// total_capital = total_shares * NAV / NAV_SCALE — the pool's total value.
    pub fn total_capital(env: Env) -> i128 {
        env.storage().instance().get(&storage::TOTAL_CAPITAL).unwrap_or(0)
    }

    pub fn financed_amount(env: Env) -> i128 {
        env.storage().instance().get(&storage::FINANCED_AMT).unwrap_or(0)
    }

    pub fn max_utilisation(env: Env) -> i128 {
        env.storage().instance().get(&storage::MAX_UTIL).unwrap_or(0)
    }

    pub fn nav(env: Env) -> i128 {
        env.storage().instance().get(&storage::NAV).unwrap_or(NAV_SCALE)
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

    /// Idle liquidity: capital not currently financed.
    pub fn reserve(env: Env) -> i128 {
        let cap: i128 = env.storage().instance().get(&storage::TOTAL_CAPITAL).unwrap_or(0);
        let fin: i128 = env.storage().instance().get(&storage::FINANCED_AMT).unwrap_or(0);
        cap - fin
    }

    /// Governance switch: excludes this pool from being picked as a rebalancing donor.
    pub fn set_donor_blocked(env: Env, blocked: bool) {
        env.storage().instance().set(&storage::DONOR_BLK, &blocked);
    }

    pub fn is_donor_blocked(env: Env) -> bool {
        env.storage().instance().get(&storage::DONOR_BLK).unwrap_or(false)
    }

    /// Applies a capital delta from a reserve rebalance (positive = received,
    /// negative = donated). NAV is re-derived so the shares/NAV/capital invariant
    /// holds; financed_amount is clamped if the new capital can no longer support it.
    pub fn apply_reserve_delta(env: Env, delta: i128) {
        let tot_shares: i128 = env.storage().instance().get(&storage::TOTAL_SHARES).unwrap_or(0);
        let tot_capital: i128 = env.storage().instance().get(&storage::TOTAL_CAPITAL).unwrap_or(0);
        let new_capital = tot_capital + delta;
        assert!(new_capital >= 0, "rebalance would leave negative capital");

        if tot_shares > 0 {
            let new_nav = new_capital * NAV_SCALE / tot_shares;
            assert!(new_nav > 0, "rebalance would zero NAV");
            env.storage().instance().set(&storage::NAV, &new_nav);
        }
        env.storage().instance().set(&storage::TOTAL_CAPITAL, &new_capital);

        let fin: i128 = env.storage().instance().get(&storage::FINANCED_AMT).unwrap_or(0);
        let max_util: i128 = env.storage().instance().get(&storage::MAX_UTIL).unwrap_or(0);
        let limit = new_capital * max_util / 10_000;
        if fin > limit {
            env.storage().instance().set(&storage::FINANCED_AMT, &limit);
        }
    }

    /// Cross-pool reserve rebalancing.
    ///
    /// Called on the initiating pool (`self`) with `peers`, the sibling PoolManager
    /// instances to consider alongside it. If any pool in `self + peers` has reserve
    /// coverage below `RESERVE_FLOOR_BPS`, capital is moved in from the pool with the
    /// highest raw reserve among pools not excluded via `set_donor_blocked`. The
    /// donation is capped at `DONOR_CAP_BPS` of the donor's excess reserve above its
    /// own floor. Emits `ReserveRebalanced` with the donor, recipient, and transferred
    /// amount, and returns whether a transfer occurred.
    ///
    /// `self` is read and updated via direct storage access rather than a
    /// cross-contract call, since a contract cannot invoke itself (the host rejects
    /// self re-entrant calls); peers are reached via `PoolManagerClient`.
    pub fn rebalance_reserves(env: Env, peers: Vec<Address>) -> bool {
        let self_addr = env.current_contract_address();
        let self_capital: i128 = env.storage().instance().get(&storage::TOTAL_CAPITAL).unwrap_or(0);
        let self_fin: i128 = env.storage().instance().get(&storage::FINANCED_AMT).unwrap_or(0);
        let self_reserve = self_capital - self_fin;
        let self_blocked: bool = env.storage().instance().get(&storage::DONOR_BLK).unwrap_or(false);

        let mut needy: Option<(Address, i128, i128)> = None; // (addr, reserve, capital)
        let mut needy_bps: i128 = RESERVE_FLOOR_BPS;
        let mut donor: Option<(Address, i128, i128)> = None; // (addr, reserve, capital)

        if self_capital > 0 {
            let reserve_bps = self_reserve * BPS_SCALE / self_capital;
            if reserve_bps < needy_bps {
                needy_bps = reserve_bps;
                needy = Some((self_addr.clone(), self_reserve, self_capital));
            }
        }
        if !self_blocked {
            donor = Some((self_addr.clone(), self_reserve, self_capital));
        }

        for peer in peers.iter() {
            let client = PoolManagerClient::new(&env, &peer);
            let capital = client.total_capital();
            let fin = client.financed_amount();
            let reserve_amt = capital - fin;

            if capital > 0 {
                let reserve_bps = reserve_amt * BPS_SCALE / capital;
                if reserve_bps < needy_bps {
                    needy_bps = reserve_bps;
                    needy = Some((peer.clone(), reserve_amt, capital));
                }
            }

            if !client.is_donor_blocked() {
                let is_higher = match &donor {
                    Some((_, best_reserve, _)) => reserve_amt > *best_reserve,
                    None => true,
                };
                if is_higher {
                    donor = Some((peer.clone(), reserve_amt, capital));
                }
            }
        }

        let (needy_addr, needy_reserve, needy_capital) = match needy {
            Some(v) => v,
            None => return false,
        };
        let (donor_addr, donor_reserve, donor_capital) = match donor {
            Some(v) => v,
            None => return false,
        };

        if donor_addr == needy_addr {
            return false;
        }

        let needy_target = needy_capital * RESERVE_FLOOR_BPS / BPS_SCALE;
        let shortfall = needy_target - needy_reserve;
        if shortfall <= 0 {
            return false;
        }

        let donor_floor = donor_capital * RESERVE_FLOOR_BPS / BPS_SCALE;
        let donor_excess = donor_reserve - donor_floor;
        if donor_excess <= 0 {
            return false;
        }
        let max_donation = donor_excess * DONOR_CAP_BPS / BPS_SCALE;

        let transfer = shortfall.min(max_donation);
        if transfer <= 0 {
            return false;
        }

        if donor_addr == self_addr {
            Self::apply_reserve_delta(env.clone(), -transfer);
        } else {
            PoolManagerClient::new(&env, &donor_addr).apply_reserve_delta(&(-transfer));
        }
        if needy_addr == self_addr {
            Self::apply_reserve_delta(env.clone(), transfer);
        } else {
            PoolManagerClient::new(&env, &needy_addr).apply_reserve_delta(&transfer);
        }

        env.events().publish(
            (symbol_short!("rebal"),),
            (donor_addr, needy_addr, transfer),
        );

        true
    }

    // ── timelocked admin actions ────────────────────────────────────────

    /// One-time bootstrap binding an `Address` that must authorize timelocked
    /// admin actions (`queue_parameter_change`, `cancel_parameter_change`).
    /// Additive: independent of the legacy `Symbol` admin tag set in
    /// `initialize`, so it does not change that function's signature.
    pub fn set_timelock_admin(env: Env, admin: Address) {
        if env.storage().instance().has(&storage::ADMIN_ADDR) {
            panic!("timelock admin already set");
        }
        env.storage().instance().set(&storage::ADMIN_ADDR, &admin);
    }

    /// Queues a change of `param` to `new_value`, executable no earlier than
    /// 48h from now. Requires the timelock admin's authorization. Returns the
    /// new action's id. Emits `ActionQueued`.
    pub fn queue_parameter_change(env: Env, param: Symbol, new_value: i128) -> u32 {
        Self::require_timelock_admin(&env);

        let id: u32 = env
            .storage()
            .instance()
            .get(&storage::NEXT_ACTION)
            .unwrap_or(0);
        env.storage().instance().set(&storage::NEXT_ACTION, &(id + 1));

        let queued_at = env.ledger().timestamp();
        let execute_after = queued_at + TIMELOCK_SECS;
        let action = QueuedAction {
            id,
            param: param.clone(),
            new_value,
            queued_at,
            execute_after,
            executed: false,
            cancelled: false,
        };
        env.storage().persistent().set(&ActionKey(id), &action);

        env.events().publish(
            (symbol_short!("act_qd"), id),
            (param, new_value, execute_after),
        );
        id
    }

    /// Executes a queued action once its timelock has elapsed. Callable by
    /// anyone — the outcome is fully determined by the queued state and the
    /// current ledger time, mirroring `governance::execute_proposal`. Emits
    /// `ActionExecuted`.
    pub fn execute_parameter_change(env: Env, id: u32) {
        let mut action = Self::read_action(&env, id);
        assert!(!action.cancelled, "action was cancelled");
        assert!(!action.executed, "action already executed");
        assert!(
            env.ledger().timestamp() >= action.execute_after,
            "timelock has not elapsed"
        );

        Self::apply_parameter(&env, &action.param, action.new_value);

        action.executed = true;
        env.storage().persistent().set(&ActionKey(id), &action);

        env.events().publish(
            (symbol_short!("act_exec"), id),
            (action.param.clone(), action.new_value),
        );
    }

    /// Cancels a queued action before it executes. Requires the timelock
    /// admin's authorization. Emits `ActionCancelled`.
    pub fn cancel_parameter_change(env: Env, id: u32) {
        Self::require_timelock_admin(&env);

        let mut action = Self::read_action(&env, id);
        assert!(!action.executed, "action already executed");
        assert!(!action.cancelled, "action already cancelled");

        action.cancelled = true;
        env.storage().persistent().set(&ActionKey(id), &action);

        env.events()
            .publish((symbol_short!("act_cxl"), id), (action.param.clone(),));
    }

    pub fn get_queued_action(env: Env, id: u32) -> Option<QueuedAction> {
        env.storage().persistent().get(&ActionKey(id))
    }

    pub fn timelock_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&storage::ADMIN_ADDR)
    }

    fn require_timelock_admin(env: &Env) -> Address {
        let admin: Address = env
            .storage()
            .instance()
            .get(&storage::ADMIN_ADDR)
            .unwrap_or_else(|| panic!("timelock admin not set"));
        admin.require_auth();
        admin
    }

    fn read_action(env: &Env, id: u32) -> QueuedAction {
        env.storage()
            .persistent()
            .get(&ActionKey(id))
            .unwrap_or_else(|| panic!("action not found"))
    }

    /// Applies a queued change for a known parameter name. `max_ut` maps to
    /// max utilisation, re-clamping `financed_amount` if the new ceiling is
    /// now below it (mirroring `set_nav`'s clamp).
    fn apply_parameter(env: &Env, param: &Symbol, new_value: i128) {
        if *param == storage::MAX_UTIL {
            env.storage().instance().set(&storage::MAX_UTIL, &new_value);

            let cap: i128 = env.storage().instance().get(&storage::TOTAL_CAPITAL).unwrap_or(0);
            let fin: i128 = env.storage().instance().get(&storage::FINANCED_AMT).unwrap_or(0);
            let limit = cap * new_value / BPS_SCALE;
            if fin > limit {
                env.storage().instance().set(&storage::FINANCED_AMT, &limit);
            }
        } else {
            panic!("unknown parameter");
        }
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
    use soroban_sdk::testutils::{Address as _, Ledger as _};
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

    // ── Reserve rebalancing ───────────────────────────────────────────

    fn deploy_pool(env: &Env, max_utilisation: i128) -> Address {
        let addr = env.register_contract(None::<&Address>, PoolManager);
        env.as_contract(&addr, || {
            PoolManager::initialize(env.clone(), symbol_short!("admin"), max_utilisation);
        });
        addr
    }

    #[test]
    fn reserve_reflects_idle_capital() {
        let (env, contract_addr) = setup();
        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), symbol_short!("alice"), 10_000);
            PoolManager::finance(env.clone(), 4_000);
            assert_eq!(PoolManager::reserve(env.clone()), 6_000);
        });
    }

    #[test]
    fn rebalance_moves_capital_from_richest_to_neediest_pool() {
        let env = Env::default();
        env.mock_all_auths();

        // Donor: 100_000 capital, financed 0 => 100% reserve.
        let donor = deploy_pool(&env, 8_000);
        env.as_contract(&donor, || {
            PoolManager::deposit(env.clone(), symbol_short!("alice"), 100_000);
        });

        // Needy: 100_000 capital, financed 98_000 => 2% reserve, below the 5% floor.
        let needy = deploy_pool(&env, 10_000);
        env.as_contract(&needy, || {
            PoolManager::deposit(env.clone(), symbol_short!("bob"), 100_000);
            PoolManager::finance(env.clone(), 98_000);
        });

        let mut peers = Vec::new(&env);
        peers.push_back(needy.clone());

        let moved = env.as_contract(&donor, || {
            PoolManager::rebalance_reserves(env.clone(), peers)
        });
        assert!(moved);

        // Needy shortfall to reach 5% of its 100_000 capital = 5_000 - 2_000 = 3_000.
        // Donor excess above its own 5% floor = 100_000 - 5_000 = 95_000, capped at 50% = 47_500.
        // Transfer = min(3_000, 47_500) = 3_000.
        env.as_contract(&needy, || {
            assert_eq!(PoolManager::total_capital(env.clone()), 103_000);
            assert_eq!(PoolManager::reserve(env.clone()), 5_000);
        });
        env.as_contract(&donor, || {
            assert_eq!(PoolManager::total_capital(env.clone()), 97_000);
        });
    }

    #[test]
    fn rebalance_is_noop_when_all_pools_healthy() {
        let env = Env::default();
        env.mock_all_auths();

        let a = deploy_pool(&env, 8_000);
        env.as_contract(&a, || {
            PoolManager::deposit(env.clone(), symbol_short!("alice"), 100_000);
        });
        let b = deploy_pool(&env, 8_000);
        env.as_contract(&b, || {
            PoolManager::deposit(env.clone(), symbol_short!("bob"), 100_000);
        });

        let mut peers = Vec::new(&env);
        peers.push_back(b.clone());

        let moved = env.as_contract(&a, || {
            PoolManager::rebalance_reserves(env.clone(), peers)
        });
        assert!(!moved);
    }

    #[test]
    fn rebalance_skips_donor_blocked_by_governance() {
        let env = Env::default();
        env.mock_all_auths();

        // Richest pool opts out of donating.
        let rich = deploy_pool(&env, 8_000);
        env.as_contract(&rich, || {
            PoolManager::deposit(env.clone(), symbol_short!("alice"), 200_000);
            PoolManager::set_donor_blocked(env.clone(), true);
        });

        // Modest pool: still has enough excess to help.
        let modest = deploy_pool(&env, 8_000);
        env.as_contract(&modest, || {
            PoolManager::deposit(env.clone(), symbol_short!("carol"), 50_000);
        });

        // Needy pool: below floor.
        let needy = deploy_pool(&env, 10_000);
        env.as_contract(&needy, || {
            PoolManager::deposit(env.clone(), symbol_short!("bob"), 100_000);
            PoolManager::finance(env.clone(), 98_000);
        });

        let mut peers = Vec::new(&env);
        peers.push_back(rich.clone());
        peers.push_back(modest.clone());

        let moved = env.as_contract(&needy, || {
            PoolManager::rebalance_reserves(env.clone(), peers)
        });
        assert!(moved);

        // The blocked pool must be untouched.
        env.as_contract(&rich, || {
            assert_eq!(PoolManager::total_capital(env.clone()), 200_000);
        });
        // The modest pool should be the one that donated.
        env.as_contract(&modest, || {
            assert_eq!(PoolManager::total_capital(env.clone()), 47_000);
        });
    }

    #[test]
    fn rebalance_transfer_capped_at_half_donor_excess() {
        let env = Env::default();
        env.mock_all_auths();

        // Small donor: capital 1_000, no financing => reserve 1_000, floor 50,
        // excess 950, 50% cap => max donation 475.
        let donor = deploy_pool(&env, 8_000);
        env.as_contract(&donor, || {
            PoolManager::deposit(env.clone(), symbol_short!("alice"), 1_000);
        });

        // Needy pool has a much larger shortfall (4_000) than the donor can cover,
        // so the 50%-of-excess cap should bind instead of the shortfall.
        let needy = deploy_pool(&env, 10_000);
        env.as_contract(&needy, || {
            PoolManager::deposit(env.clone(), symbol_short!("bob"), 100_000);
            PoolManager::finance(env.clone(), 99_000); // reserve = 1_000 (1%)
        });

        let mut peers = Vec::new(&env);
        peers.push_back(needy.clone());

        let moved = env.as_contract(&donor, || {
            PoolManager::rebalance_reserves(env.clone(), peers)
        });
        assert!(moved);

        // Transfer = min(shortfall 4_000, cap 475) = 475.
        env.as_contract(&donor, || {
            assert_eq!(PoolManager::total_capital(env.clone()), 525);
        });
        env.as_contract(&needy, || {
            assert_eq!(PoolManager::total_capital(env.clone()), 100_475);
            assert_eq!(PoolManager::reserve(env.clone()), 1_475);
        });
    }

    // ── Timelocked admin actions ──────────────────────────────────────

    fn advance_time(env: &Env, by_secs: u64) {
        let now = env.ledger().timestamp();
        env.ledger().with_mut(|li| li.timestamp = now + by_secs);
    }

    #[test]
    fn queue_execute_and_read_a_parameter_change() {
        let (env, contract_addr) = setup();
        let admin_addr = Address::generate(&env);

        let id = env.as_contract(&contract_addr, || {
            PoolManager::set_timelock_admin(env.clone(), admin_addr.clone());
            PoolManager::queue_parameter_change(env.clone(), storage::MAX_UTIL, 9_000)
        });

        env.as_contract(&contract_addr, || {
            let action = PoolManager::get_queued_action(env.clone(), id).unwrap();
            assert_eq!(action.new_value, 9_000);
            assert!(!action.executed);
            assert!(!action.cancelled);
            assert_eq!(action.execute_after - action.queued_at, 48 * 60 * 60);
        });

        advance_time(&env, 48 * 60 * 60);

        env.as_contract(&contract_addr, || {
            PoolManager::execute_parameter_change(env.clone(), id);
            assert_eq!(PoolManager::max_utilisation(env.clone()), 9_000);
            let action = PoolManager::get_queued_action(env.clone(), id).unwrap();
            assert!(action.executed);
        });
    }

    #[test]
    #[should_panic(expected = "timelock has not elapsed")]
    fn execute_before_timelock_elapses_panics() {
        let (env, contract_addr) = setup();
        let admin_addr = Address::generate(&env);

        let id = env.as_contract(&contract_addr, || {
            PoolManager::set_timelock_admin(env.clone(), admin_addr);
            PoolManager::queue_parameter_change(env.clone(), storage::MAX_UTIL, 9_000)
        });

        advance_time(&env, 47 * 60 * 60);

        env.as_contract(&contract_addr, || {
            PoolManager::execute_parameter_change(env.clone(), id);
        });
    }

    #[test]
    fn cancel_marks_the_action_cancelled() {
        let (env, contract_addr) = setup();
        let admin_addr = Address::generate(&env);

        let id = env.as_contract(&contract_addr, || {
            PoolManager::set_timelock_admin(env.clone(), admin_addr);
            PoolManager::queue_parameter_change(env.clone(), storage::MAX_UTIL, 9_000)
        });

        env.as_contract(&contract_addr, || {
            PoolManager::cancel_parameter_change(env.clone(), id);
            let action = PoolManager::get_queued_action(env.clone(), id).unwrap();
            assert!(action.cancelled);
        });
    }

    #[test]
    #[should_panic(expected = "action was cancelled")]
    fn execute_after_cancel_panics() {
        let (env, contract_addr) = setup();
        let admin_addr = Address::generate(&env);

        let id = env.as_contract(&contract_addr, || {
            PoolManager::set_timelock_admin(env.clone(), admin_addr);
            PoolManager::queue_parameter_change(env.clone(), storage::MAX_UTIL, 9_000)
        });

        env.as_contract(&contract_addr, || {
            PoolManager::cancel_parameter_change(env.clone(), id);
        });

        advance_time(&env, 48 * 60 * 60);

        env.as_contract(&contract_addr, || {
            PoolManager::execute_parameter_change(env.clone(), id);
        });
    }

    #[test]
    fn execute_reclamps_financed_amount_when_ceiling_drops() {
        let (env, contract_addr) = setup();
        let admin_addr = Address::generate(&env);
        let alice = symbol_short!("alice");

        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), alice, 100_000);
            PoolManager::finance(env.clone(), 75_000); // within the initial 80% ceiling
        });

        let id = env.as_contract(&contract_addr, || {
            PoolManager::set_timelock_admin(env.clone(), admin_addr);
            PoolManager::queue_parameter_change(env.clone(), storage::MAX_UTIL, 5_000)
        });

        advance_time(&env, 48 * 60 * 60);

        env.as_contract(&contract_addr, || {
            PoolManager::execute_parameter_change(env.clone(), id);
            assert_eq!(PoolManager::max_utilisation(env.clone()), 5_000);
            assert_eq!(PoolManager::financed_amount(env.clone()), 50_000);
        });
    }

    #[test]
    #[should_panic(expected = "timelock admin not set")]
    fn queue_without_timelock_admin_set_panics() {
        let (env, contract_addr) = setup();
        env.as_contract(&contract_addr, || {
            PoolManager::queue_parameter_change(env.clone(), storage::MAX_UTIL, 9_000);
        });
    }

    #[test]
    #[should_panic(expected = "timelock admin already set")]
    fn set_timelock_admin_is_one_time() {
        let (env, contract_addr) = setup();
        let a = Address::generate(&env);
        let b = Address::generate(&env);
        env.as_contract(&contract_addr, || {
            PoolManager::set_timelock_admin(env.clone(), a);
            PoolManager::set_timelock_admin(env.clone(), b);
        });
    }
}
