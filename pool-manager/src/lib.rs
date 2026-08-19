#![no_std]
#[allow(unused_imports)]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, Symbol, Vec,
};

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

    // ── Pool creation config (issue #17) ────────────────────────────────
    /// Whether `create_pool` has already been called (guards duplicate config).
    pub const POOL_CFG_SET: Symbol = symbol_short!("pl_cfg");
    pub const BUYER_LIMIT_BPS: Symbol = symbol_short!("buy_lim");
    pub const SME_LIMIT_BPS: Symbol = symbol_short!("sme_lim");
    pub const MIN_DEPOSIT: Symbol = symbol_short!("min_dep");
    pub const MAX_DEPOSIT: Symbol = symbol_short!("max_dep");
    pub const RESERVE_RATIO_BPS: Symbol = symbol_short!("rsv_rat");
}

/// Errors surfaced by the pool manager. Stable `u32` discriminants so
/// integrators and audit tooling can match on them.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    /// `initialize` called on an already-initialized contract.
    AlreadyInitialized = 1,
    /// A state-changing call was made before `initialize`.
    NotInitialized = 2,
    /// A deposit or finance amount was not strictly positive.
    InvalidAmount = 3,
    /// A withdrawal share count was not strictly positive.
    InvalidShares = 4,
    /// The lender tried to withdraw more shares than they hold.
    InsufficientShares = 5,
    /// Financing would push financed_amount above the utilisation ceiling.
    MaxUtilisationExceeded = 6,
    /// A NAV update was not strictly positive.
    InvalidNav = 7,
    /// A reserve rebalance would leave total capital negative.
    NegativeCapital = 8,
    /// A reserve rebalance would drive NAV to zero.
    ZeroNav = 9,
    /// `set_timelock_admin` called after an admin was already bound.
    TimelockAdminAlreadySet = 10,
    /// A timelocked action was queued before `set_timelock_admin`.
    TimelockAdminNotSet = 11,
    /// The referenced queued action id does not exist.
    ActionNotFound = 12,
    /// `execute` was called on an already-cancelled action.
    ActionCancelled = 13,
    /// `execute`/`cancel` was called on an already-executed action.
    ActionAlreadyExecuted = 14,
    /// `cancel` was called on an already-cancelled action.
    ActionAlreadyCancelled = 15,
    /// `execute` was called before the action's timelock window elapsed.
    TimelockNotElapsed = 16,
    /// A queued change referenced an unknown parameter name.
    UnknownParameter = 17,
    /// `create_pool` called before `initialize`.
    PoolNotInitialized = 18,
    /// `create_pool` called more than once.
    PoolAlreadyCreated = 19,
    /// A concentration/reserve bps parameter was outside `(0, BPS_SCALE]`.
    InvalidBps = 20,
    /// `min_deposit` was not strictly positive, or exceeded `max_deposit`.
    InvalidDepositRange = 21,
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
    pub fn initialize(env: Env, admin: Symbol, max_utilisation: i128) -> Result<(), ContractError> {
        if env.storage().instance().has(&storage::ADMIN) {
            return Err(ContractError::AlreadyInitialized);
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
        Ok(())
    }

    // ── Pool creation config (issue #17) ────────────────────────────────

    /// Configures per-buyer / per-SME concentration limits and deposit/reserve
    /// parameters for this pool. Callable once, after `initialize`. Errors
    /// if the pool wasn't initialized yet, or if configuration was already
    /// set (mirrors "duplicate pool IDs rejected" for this single-pool
    /// contract instance). Emits `PoolCreated`.
    pub fn create_pool(
        env: Env,
        buyer_limit_bps: i128,
        sme_limit_bps: i128,
        min_deposit: i128,
        max_deposit: i128,
        reserve_ratio_bps: i128,
    ) -> Result<(), ContractError> {
        if !env.storage().instance().has(&storage::ADMIN) {
            return Err(ContractError::PoolNotInitialized);
        }
        if env.storage().instance().has(&storage::POOL_CFG_SET) {
            return Err(ContractError::PoolAlreadyCreated);
        }
        if buyer_limit_bps <= 0 || buyer_limit_bps > BPS_SCALE {
            return Err(ContractError::InvalidBps);
        }
        if sme_limit_bps <= 0 || sme_limit_bps > BPS_SCALE {
            return Err(ContractError::InvalidBps);
        }
        if reserve_ratio_bps < 0 || reserve_ratio_bps > BPS_SCALE {
            return Err(ContractError::InvalidBps);
        }
        if min_deposit <= 0 || min_deposit > max_deposit {
            return Err(ContractError::InvalidDepositRange);
        }

        env.storage().instance().set(&storage::POOL_CFG_SET, &true);
        env.storage()
            .instance()
            .set(&storage::BUYER_LIMIT_BPS, &buyer_limit_bps);
        env.storage()
            .instance()
            .set(&storage::SME_LIMIT_BPS, &sme_limit_bps);
        env.storage()
            .instance()
            .set(&storage::MIN_DEPOSIT, &min_deposit);
        env.storage()
            .instance()
            .set(&storage::MAX_DEPOSIT, &max_deposit);
        env.storage()
            .instance()
            .set(&storage::RESERVE_RATIO_BPS, &reserve_ratio_bps);

        env.events().publish(
            (symbol_short!("pool_new"),),
            (buyer_limit_bps, sme_limit_bps, min_deposit, max_deposit, reserve_ratio_bps),
        );
        Ok(())
    }

    pub fn buyer_limit_bps(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&storage::BUYER_LIMIT_BPS)
            .unwrap_or(0)
    }

    pub fn sme_limit_bps(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&storage::SME_LIMIT_BPS)
            .unwrap_or(0)
    }

    pub fn min_deposit(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&storage::MIN_DEPOSIT)
            .unwrap_or(0)
    }

    pub fn max_deposit(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&storage::MAX_DEPOSIT)
            .unwrap_or(0)
    }

    pub fn reserve_ratio_bps(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&storage::RESERVE_RATIO_BPS)
            .unwrap_or(0)
    }

    // ── mutators ──────────────────────────────────────────────────────

    /// Deposit capital into the pool. `amount` is in base units.
    /// Shares minted = amount * NAV_SCALE / nav.
    /// Returns the number of shares minted.
    pub fn deposit(env: Env, lender: Symbol, amount: i128) -> Result<i128, ContractError> {
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let nav: i128 = env
            .storage()
            .instance()
            .get(&storage::NAV)
            .ok_or(ContractError::NotInitialized)?;
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
            .ok_or(ContractError::NotInitialized)?;
        let new_tot_shares = tot_shares + shares;

        env.storage()
            .instance()
            .set(&storage::TOTAL_SHARES, &new_tot_shares);
        env.storage()
            .instance()
            .set(&storage::TOTAL_CAPITAL, &(new_tot_shares * nav / NAV_SCALE));

        Ok(shares)
    }

    // ── join_pool (issue #18) ────────────────────────────────────────

    /// Deposit capital into the pool as an authenticated `Address` lender,
    /// receiving LP shares. Requires `lender.require_auth()`. First deposit
    /// prices shares 1:1 (NAV starts at `NAV_SCALE`); subsequent deposits
    /// mint at the current NAV. Emits `SharesMinted` and returns the number
    /// of shares minted.
    ///
    /// Ledger accounting is shared with [`Self::deposit`] (same NAV math,
    /// same `LenderPosition`/totals storage) — this is an additive,
    /// auth-checked entrypoint alongside it, keyed by the lender's `Symbol`
    /// tag derived from their address so both entrypoints stay consistent
    /// against the same lender ledger.
    pub fn join_pool(
        env: Env,
        lender: Address,
        lender_tag: Symbol,
        amount: i128,
    ) -> Result<i128, ContractError> {
        lender.require_auth();
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let nav: i128 = env
            .storage()
            .instance()
            .get(&storage::NAV)
            .ok_or(ContractError::NotInitialized)?;
        let shares = amount * NAV_SCALE / nav;

        let key = LenderKey(lender_tag);
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

        let tot_shares: i128 = env
            .storage()
            .instance()
            .get(&storage::TOTAL_SHARES)
            .ok_or(ContractError::NotInitialized)?;
        let new_tot_shares = tot_shares + shares;

        env.storage()
            .instance()
            .set(&storage::TOTAL_SHARES, &new_tot_shares);
        env.storage()
            .instance()
            .set(&storage::TOTAL_CAPITAL, &(new_tot_shares * nav / NAV_SCALE));

        env.events()
            .publish((symbol_short!("shr_mint"), lender), shares);

        Ok(shares)
    }

    /// Withdraw capital. Returns the amount withdrawn in base units.
    pub fn withdraw(env: Env, lender: Symbol, shares: i128) -> Result<i128, ContractError> {
        if shares <= 0 {
            return Err(ContractError::InvalidShares);
        }

        let key = LenderKey(lender);
        let pos: LenderPosition = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(LenderPosition { shares: 0 });
        if pos.shares < shares {
            return Err(ContractError::InsufficientShares);
        }

        let nav: i128 = env
            .storage()
            .instance()
            .get(&storage::NAV)
            .ok_or(ContractError::NotInitialized)?;
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
            .ok_or(ContractError::NotInitialized)?;
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
            .ok_or(ContractError::NotInitialized)?;
        let max_util: i128 = env
            .storage()
            .instance()
            .get(&storage::MAX_UTIL)
            .ok_or(ContractError::NotInitialized)?;
        let limit = new_capital * max_util / 10_000;
        if fin > limit {
            env.storage().instance().set(&storage::FINANCED_AMT, &limit);
        }

        Ok(amount)
    }

    /// Mark an invoice as financed. `amount` is the financed value.
    /// Invariant enforced: financed_amount <= total_capital * max_utilisation / 10_000
    pub fn finance(env: Env, amount: i128) -> Result<(), ContractError> {
        if amount <= 0 {
            return Err(ContractError::InvalidAmount);
        }

        let fin: i128 = env
            .storage()
            .instance()
            .get(&storage::FINANCED_AMT)
            .ok_or(ContractError::NotInitialized)?;
        let tot_capital: i128 = env
            .storage()
            .instance()
            .get(&storage::TOTAL_CAPITAL)
            .ok_or(ContractError::NotInitialized)?;
        let max_util: i128 = env
            .storage()
            .instance()
            .get(&storage::MAX_UTIL)
            .ok_or(ContractError::NotInitialized)?;

        let new_fin = fin + amount;
        if new_fin > tot_capital * max_util / 10_000 {
            return Err(ContractError::MaxUtilisationExceeded);
        }

        env.storage()
            .instance()
            .set(&storage::FINANCED_AMT, &new_fin);
        Ok(())
    }

    /// Update the NAV (net asset value) per share. `new_nav` is scaled by NAV_SCALE.
    /// total_capital is re-derived from the canonical invariant.
    /// If the new capital drops below the financed amount limit, financed_amount is clamped.
    pub fn set_nav(env: Env, new_nav: i128) -> Result<(), ContractError> {
        if new_nav <= 0 {
            return Err(ContractError::InvalidNav);
        }

        let tot_shares: i128 = env
            .storage()
            .instance()
            .get(&storage::TOTAL_SHARES)
            .ok_or(ContractError::NotInitialized)?;
        let max_util: i128 = env
            .storage()
            .instance()
            .get(&storage::MAX_UTIL)
            .ok_or(ContractError::NotInitialized)?;
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
            .ok_or(ContractError::NotInitialized)?;
        let limit = new_capital * max_util / 10_000;
        if fin > limit {
            env.storage().instance().set(&storage::FINANCED_AMT, &limit);
        }
        Ok(())
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

    /// Idle liquidity: capital not currently financed.
    pub fn reserve(env: Env) -> i128 {
        let cap: i128 = env
            .storage()
            .instance()
            .get(&storage::TOTAL_CAPITAL)
            .unwrap_or(0);
        let fin: i128 = env
            .storage()
            .instance()
            .get(&storage::FINANCED_AMT)
            .unwrap_or(0);
        cap - fin
    }

    /// Governance switch: excludes this pool from being picked as a rebalancing donor.
    pub fn set_donor_blocked(env: Env, blocked: bool) {
        env.storage().instance().set(&storage::DONOR_BLK, &blocked);
    }

    pub fn is_donor_blocked(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&storage::DONOR_BLK)
            .unwrap_or(false)
    }

    /// Applies a capital delta from a reserve rebalance (positive = received,
    /// negative = donated). NAV is re-derived so the shares/NAV/capital invariant
    /// holds; financed_amount is clamped if the new capital can no longer support it.
    pub fn apply_reserve_delta(env: Env, delta: i128) -> Result<(), ContractError> {
        let tot_shares: i128 = env
            .storage()
            .instance()
            .get(&storage::TOTAL_SHARES)
            .unwrap_or(0);
        let tot_capital: i128 = env
            .storage()
            .instance()
            .get(&storage::TOTAL_CAPITAL)
            .unwrap_or(0);
        let new_capital = tot_capital + delta;
        if new_capital < 0 {
            return Err(ContractError::NegativeCapital);
        }

        if tot_shares > 0 {
            let new_nav = new_capital * NAV_SCALE / tot_shares;
            if new_nav <= 0 {
                return Err(ContractError::ZeroNav);
            }
            env.storage().instance().set(&storage::NAV, &new_nav);
        }
        env.storage()
            .instance()
            .set(&storage::TOTAL_CAPITAL, &new_capital);

        let fin: i128 = env
            .storage()
            .instance()
            .get(&storage::FINANCED_AMT)
            .unwrap_or(0);
        let max_util: i128 = env
            .storage()
            .instance()
            .get(&storage::MAX_UTIL)
            .unwrap_or(0);
        let limit = new_capital * max_util / 10_000;
        if fin > limit {
            env.storage().instance().set(&storage::FINANCED_AMT, &limit);
        }
        Ok(())
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
    pub fn rebalance_reserves(env: Env, peers: Vec<Address>) -> Result<bool, ContractError> {
        let self_addr = env.current_contract_address();
        let self_capital: i128 = env
            .storage()
            .instance()
            .get(&storage::TOTAL_CAPITAL)
            .unwrap_or(0);
        let self_fin: i128 = env
            .storage()
            .instance()
            .get(&storage::FINANCED_AMT)
            .unwrap_or(0);
        let self_reserve = self_capital - self_fin;
        let self_blocked: bool = env
            .storage()
            .instance()
            .get(&storage::DONOR_BLK)
            .unwrap_or(false);

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
            None => return Ok(false),
        };
        let (donor_addr, donor_reserve, donor_capital) = match donor {
            Some(v) => v,
            None => return Ok(false),
        };

        if donor_addr == needy_addr {
            return Ok(false);
        }

        let needy_target = needy_capital * RESERVE_FLOOR_BPS / BPS_SCALE;
        let shortfall = needy_target - needy_reserve;
        if shortfall <= 0 {
            return Ok(false);
        }

        let donor_floor = donor_capital * RESERVE_FLOOR_BPS / BPS_SCALE;
        let donor_excess = donor_reserve - donor_floor;
        if donor_excess <= 0 {
            return Ok(false);
        }
        let max_donation = donor_excess * DONOR_CAP_BPS / BPS_SCALE;

        let transfer = shortfall.min(max_donation);
        if transfer <= 0 {
            return Ok(false);
        }

        if donor_addr == self_addr {
            Self::apply_reserve_delta(env.clone(), -transfer)?;
        } else {
            PoolManagerClient::new(&env, &donor_addr).apply_reserve_delta(&(-transfer));
        }
        if needy_addr == self_addr {
            Self::apply_reserve_delta(env.clone(), transfer)?;
        } else {
            PoolManagerClient::new(&env, &needy_addr).apply_reserve_delta(&transfer);
        }

        env.events().publish(
            (symbol_short!("rebal"),),
            (donor_addr, needy_addr, transfer),
        );

        Ok(true)
    }

    // ── timelocked admin actions ────────────────────────────────────────

    /// One-time bootstrap binding an `Address` that must authorize timelocked
    /// admin actions (`queue_parameter_change`, `cancel_parameter_change`).
    /// Additive: independent of the legacy `Symbol` admin tag set in
    /// `initialize`, so it does not change that function's signature.
    pub fn set_timelock_admin(env: Env, admin: Address) -> Result<(), ContractError> {
        if env.storage().instance().has(&storage::ADMIN_ADDR) {
            return Err(ContractError::TimelockAdminAlreadySet);
        }
        env.storage().instance().set(&storage::ADMIN_ADDR, &admin);
        Ok(())
    }

    /// Queues a change of `param` to `new_value`, executable no earlier than
    /// 48h from now. Requires the timelock admin's authorization. Returns the
    /// new action's id. Emits `ActionQueued`.
    pub fn queue_parameter_change(
        env: Env,
        param: Symbol,
        new_value: i128,
    ) -> Result<u32, ContractError> {
        Self::require_timelock_admin(&env)?;

        let id: u32 = env
            .storage()
            .instance()
            .get(&storage::NEXT_ACTION)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&storage::NEXT_ACTION, &(id + 1));

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
        Ok(id)
    }

    /// Executes a queued action once its timelock has elapsed. Callable by
    /// anyone — the outcome is fully determined by the queued state and the
    /// current ledger time, mirroring `governance::execute_proposal`. Emits
    /// `ActionExecuted`.
    pub fn execute_parameter_change(env: Env, id: u32) -> Result<(), ContractError> {
        let mut action = Self::read_action(&env, id)?;
        if action.cancelled {
            return Err(ContractError::ActionCancelled);
        }
        if action.executed {
            return Err(ContractError::ActionAlreadyExecuted);
        }
        if env.ledger().timestamp() < action.execute_after {
            return Err(ContractError::TimelockNotElapsed);
        }

        Self::apply_parameter(&env, &action.param, action.new_value)?;

        action.executed = true;
        env.storage().persistent().set(&ActionKey(id), &action);

        env.events().publish(
            (symbol_short!("act_exec"), id),
            (action.param.clone(), action.new_value),
        );
        Ok(())
    }

    /// Cancels a queued action before it executes. Requires the timelock
    /// admin's authorization. Emits `ActionCancelled`.
    pub fn cancel_parameter_change(env: Env, id: u32) -> Result<(), ContractError> {
        Self::require_timelock_admin(&env)?;

        let mut action = Self::read_action(&env, id)?;
        if action.executed {
            return Err(ContractError::ActionAlreadyExecuted);
        }
        if action.cancelled {
            return Err(ContractError::ActionAlreadyCancelled);
        }

        action.cancelled = true;
        env.storage().persistent().set(&ActionKey(id), &action);

        env.events()
            .publish((symbol_short!("act_cxl"), id), (action.param.clone(),));
        Ok(())
    }

    pub fn get_queued_action(env: Env, id: u32) -> Option<QueuedAction> {
        env.storage().persistent().get(&ActionKey(id))
    }

    pub fn timelock_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&storage::ADMIN_ADDR)
    }

    fn require_timelock_admin(env: &Env) -> Result<Address, ContractError> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&storage::ADMIN_ADDR)
            .ok_or(ContractError::TimelockAdminNotSet)?;
        admin.require_auth();
        Ok(admin)
    }

    fn read_action(env: &Env, id: u32) -> Result<QueuedAction, ContractError> {
        env.storage()
            .persistent()
            .get(&ActionKey(id))
            .ok_or(ContractError::ActionNotFound)
    }

    /// Applies a queued change for a known parameter name. `max_ut` maps to
    /// max utilisation, re-clamping `financed_amount` if the new ceiling is
    /// now below it (mirroring `set_nav`'s clamp).
    fn apply_parameter(env: &Env, param: &Symbol, new_value: i128) -> Result<(), ContractError> {
        if *param == storage::MAX_UTIL {
            env.storage().instance().set(&storage::MAX_UTIL, &new_value);

            let cap: i128 = env
                .storage()
                .instance()
                .get(&storage::TOTAL_CAPITAL)
                .unwrap_or(0);
            let fin: i128 = env
                .storage()
                .instance()
                .get(&storage::FINANCED_AMT)
                .unwrap_or(0);
            let limit = cap * new_value / BPS_SCALE;
            if fin > limit {
                env.storage().instance().set(&storage::FINANCED_AMT, &limit);
            }
            Ok(())
        } else {
            Err(ContractError::UnknownParameter)
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
            PoolManager::initialize(env.clone(), symbol_short!("admin"), 8_000).unwrap();
        });
        (env, contract_addr)
    }

    // ── Pool creation config ──────────────────────────────────────────

    #[test]
    fn create_pool_stores_limits_and_config() {
        let (env, contract_addr) = setup();
        env.as_contract(&contract_addr, || {
            PoolManager::create_pool(env.clone(), 2_000, 1_000, 100, 1_000_000, 500).unwrap();
        });

        env.as_contract(&contract_addr, || {
            assert_eq!(PoolManager::buyer_limit_bps(env.clone()), 2_000);
            assert_eq!(PoolManager::sme_limit_bps(env.clone()), 1_000);
            assert_eq!(PoolManager::min_deposit(env.clone()), 100);
            assert_eq!(PoolManager::max_deposit(env.clone()), 1_000_000);
            assert_eq!(PoolManager::reserve_ratio_bps(env.clone()), 500);
        });
    }

    #[test]
    fn create_pool_rejects_duplicate_configuration() {
        let (env, contract_addr) = setup();
        let err = env.as_contract(&contract_addr, || {
            PoolManager::create_pool(env.clone(), 2_000, 1_000, 100, 1_000_000, 500).unwrap();
            PoolManager::create_pool(env.clone(), 2_000, 1_000, 100, 1_000_000, 500)
        });
        assert_eq!(err, Err(ContractError::PoolAlreadyCreated));
    }

    #[test]
    fn create_pool_requires_prior_initialize() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_addr = env.register_contract(None::<&Address>, PoolManager);
        let err = env.as_contract(&contract_addr, || {
            PoolManager::create_pool(env.clone(), 2_000, 1_000, 100, 1_000_000, 500)
        });
        assert_eq!(err, Err(ContractError::PoolNotInitialized));
  }
    // ── join_pool ──────────────────────────────────────────────────────

    #[test]
    fn join_pool_first_deposit_prices_shares_one_to_one() {
        let (env, contract_addr) = setup();
        let alice = Address::generate(&env);

        let shares = env.as_contract(&contract_addr, || {
            PoolManager::join_pool(env.clone(), alice, symbol_short!("alice"), 10_000).unwrap()
        });
        assert_eq!(shares, 10_000);
    }

    #[test]
    fn join_pool_subsequent_deposit_mints_at_current_nav() {
        let (env, contract_addr) = setup();
        let alice = Address::generate(&env);
        let bob = Address::generate(&env);

        env.as_contract(&contract_addr, || {
            PoolManager::join_pool(env.clone(), alice, symbol_short!("alice"), 10_000).unwrap();
            PoolManager::set_nav(env.clone(), 2_000_000).unwrap(); // NAV doubles
        });

        let bob_shares = env.as_contract(&contract_addr, || {
            PoolManager::join_pool(env.clone(), bob, symbol_short!("bob"), 10_000).unwrap()
        });
        // At 2x NAV, 10_000 deposited buys half as many shares.
        assert_eq!(bob_shares, 5_000);
    }

    #[test]
    #[should_panic]
    fn join_pool_requires_auth_from_lender() {
        let env = Env::default();
        let _admin = Address::generate(&env);
        let contract_addr = env.register_contract(None::<&Address>, PoolManager);
        env.as_contract(&contract_addr, || {
            PoolManager::initialize(env.clone(), symbol_short!("admin"), 8_000).unwrap();
        });

        // No mock_all_auths(): the lender never authorized this call.
        let alice = Address::generate(&env);
        env.as_contract(&contract_addr, || {
            PoolManager::join_pool(env.clone(), alice, symbol_short!("alice"), 10_000).unwrap();
        });
    }

    // ── Invariant 1: total_shares * NAV / NAV_SCALE == total_capital ─

    #[test]
    fn invariant_shares_nav_equals_capital_after_deposits() {
        let (env, contract_addr) = setup();
        let alice = symbol_short!("alice");
        let bob = symbol_short!("bob");

        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), alice.clone(), 10_000).unwrap();
        });
        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), bob, 5_000).unwrap();
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
            PoolManager::deposit(env.clone(), alice.clone(), 20_000).unwrap();
        });

        let shares_to_withdraw = env.as_contract(&contract_addr, || {
            PoolManager::lender_shares(env.clone(), alice.clone()) / 2
        });
        env.as_contract(&contract_addr, || {
            PoolManager::withdraw(env.clone(), alice, shares_to_withdraw).unwrap();
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
            PoolManager::deposit(env.clone(), alice, 10_000).unwrap();
        });

        env.as_contract(&contract_addr, || {
            PoolManager::set_nav(env.clone(), 1_200_000).unwrap();
        });

        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), bob, 5_000).unwrap();
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
            let sh = PoolManager::deposit(env.clone(), alice.clone(), 10_000).unwrap();
            assert_eq!(sh, 10_000);
        });

        env.as_contract(&contract_addr, || {
            let sh = PoolManager::deposit(env.clone(), bob.clone(), 5_000).unwrap();
            assert_eq!(sh, 5_000);
        });

        env.as_contract(&contract_addr, || {
            assert_eq!(PoolManager::total_shares(env.clone()), 15_000);
            assert_eq!(PoolManager::total_capital(env.clone()), 15_000);
        });

        env.as_contract(&contract_addr, || {
            let withdrawn = PoolManager::withdraw(env.clone(), alice, 10_000).unwrap();
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
            PoolManager::deposit(env.clone(), alice, 100_000).unwrap();
        });

        env.as_contract(&contract_addr, || {
            PoolManager::finance(env.clone(), 80_000).unwrap();
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
            PoolManager::deposit(env.clone(), alice.clone(), 50_000).unwrap();
            PoolManager::deposit(env.clone(), bob, 50_000).unwrap();
            PoolManager::finance(env.clone(), 40_000).unwrap();
        });

        let sh = env.as_contract(&contract_addr, || {
            PoolManager::lender_shares(env.clone(), alice.clone())
        });
        env.as_contract(&contract_addr, || {
            PoolManager::withdraw(env.clone(), alice, sh * 3 / 5).unwrap();
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
            PoolManager::deposit(env.clone(), alice.clone(), 30_000).unwrap();
            PoolManager::deposit(env.clone(), bob.clone(), 70_000).unwrap();
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
            PoolManager::deposit(env.clone(), alice.clone(), 10_000).unwrap();
        });

        let alice_shares = env.as_contract(&contract_addr, || {
            PoolManager::lender_shares(env.clone(), alice.clone())
        });

        env.as_contract(&contract_addr, || {
            PoolManager::withdraw(env.clone(), alice.clone(), alice_shares).unwrap();
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
            PoolManager::initialize(env.clone(), symbol_short!("admin"), 8_000).unwrap();
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
                    PoolManager::deposit(env.clone(), lender, amount).unwrap();
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
                            PoolManager::withdraw(env.clone(), lender, withdraw_sh).unwrap();
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
                        PoolManager::finance(env.clone(), amount).unwrap();
                    }
                });
            } else {
                // 5% change NAV (range 500_000 to 2_500_000 i.e. 0.5x to 2.5x)
                let new_nav = ((rng_state >> 16) % 2_000_000 + 500_000) as i128;
                env.as_contract(&contract_addr, || {
                    PoolManager::set_nav(env.clone(), new_nav).unwrap();
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
            PoolManager::initialize(env.clone(), symbol_short!("admin"), max_utilisation).unwrap();
        });
        addr
    }

    #[test]
    fn reserve_reflects_idle_capital() {
        let (env, contract_addr) = setup();
        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), symbol_short!("alice"), 10_000).unwrap();
            PoolManager::finance(env.clone(), 4_000).unwrap();
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
            PoolManager::deposit(env.clone(), symbol_short!("alice"), 100_000).unwrap();
        });

        // Needy: 100_000 capital, financed 98_000 => 2% reserve, below the 5% floor.
        let needy = deploy_pool(&env, 10_000);
        env.as_contract(&needy, || {
            PoolManager::deposit(env.clone(), symbol_short!("bob"), 100_000).unwrap();
            PoolManager::finance(env.clone(), 98_000).unwrap();
        });

        let mut peers = Vec::new(&env);
        peers.push_back(needy.clone());

        let moved = env.as_contract(&donor, || {
            PoolManager::rebalance_reserves(env.clone(), peers).unwrap()
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
            PoolManager::deposit(env.clone(), symbol_short!("alice"), 100_000).unwrap();
        });
        let b = deploy_pool(&env, 8_000);
        env.as_contract(&b, || {
            PoolManager::deposit(env.clone(), symbol_short!("bob"), 100_000).unwrap();
        });

        let mut peers = Vec::new(&env);
        peers.push_back(b.clone());

        let moved = env.as_contract(&a, || {
            PoolManager::rebalance_reserves(env.clone(), peers).unwrap()
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
            PoolManager::deposit(env.clone(), symbol_short!("alice"), 200_000).unwrap();
            PoolManager::set_donor_blocked(env.clone(), true);
        });

        // Modest pool: still has enough excess to help.
        let modest = deploy_pool(&env, 8_000);
        env.as_contract(&modest, || {
            PoolManager::deposit(env.clone(), symbol_short!("carol"), 50_000).unwrap();
        });

        // Needy pool: below floor.
        let needy = deploy_pool(&env, 10_000);
        env.as_contract(&needy, || {
            PoolManager::deposit(env.clone(), symbol_short!("bob"), 100_000).unwrap();
            PoolManager::finance(env.clone(), 98_000).unwrap();
        });

        let mut peers = Vec::new(&env);
        peers.push_back(rich.clone());
        peers.push_back(modest.clone());

        let moved = env.as_contract(&needy, || {
            PoolManager::rebalance_reserves(env.clone(), peers).unwrap()
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
            PoolManager::deposit(env.clone(), symbol_short!("alice"), 1_000).unwrap();
        });

        // Needy pool has a much larger shortfall (4_000) than the donor can cover,
        // so the 50%-of-excess cap should bind instead of the shortfall.
        let needy = deploy_pool(&env, 10_000);
        env.as_contract(&needy, || {
            PoolManager::deposit(env.clone(), symbol_short!("bob"), 100_000).unwrap();
            PoolManager::finance(env.clone(), 99_000).unwrap(); // reserve = 1_000 (1%)
        });

        let mut peers = Vec::new(&env);
        peers.push_back(needy.clone());

        let moved = env.as_contract(&donor, || {
            PoolManager::rebalance_reserves(env.clone(), peers).unwrap()
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
            PoolManager::set_timelock_admin(env.clone(), admin_addr.clone()).unwrap();
            PoolManager::queue_parameter_change(env.clone(), storage::MAX_UTIL, 9_000).unwrap()
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
            PoolManager::execute_parameter_change(env.clone(), id).unwrap();
            assert_eq!(PoolManager::max_utilisation(env.clone()), 9_000);
            let action = PoolManager::get_queued_action(env.clone(), id).unwrap();
            assert!(action.executed);
        });
    }

    #[test]
    fn execute_before_timelock_elapses_errors() {
        let (env, contract_addr) = setup();
        let admin_addr = Address::generate(&env);

        let id = env.as_contract(&contract_addr, || {
            PoolManager::set_timelock_admin(env.clone(), admin_addr).unwrap();
            PoolManager::queue_parameter_change(env.clone(), storage::MAX_UTIL, 9_000).unwrap()
        });

        advance_time(&env, 47 * 60 * 60);

        let err = env.as_contract(&contract_addr, || {
            PoolManager::execute_parameter_change(env.clone(), id)
        });
        assert_eq!(err, Err(ContractError::TimelockNotElapsed));
    }

    #[test]
    fn cancel_marks_the_action_cancelled() {
        let (env, contract_addr) = setup();
        let admin_addr = Address::generate(&env);

        let id = env.as_contract(&contract_addr, || {
            PoolManager::set_timelock_admin(env.clone(), admin_addr).unwrap();
            PoolManager::queue_parameter_change(env.clone(), storage::MAX_UTIL, 9_000).unwrap()
        });

        env.as_contract(&contract_addr, || {
            PoolManager::cancel_parameter_change(env.clone(), id).unwrap();
            let action = PoolManager::get_queued_action(env.clone(), id).unwrap();
            assert!(action.cancelled);
        });
    }

    #[test]
    fn execute_after_cancel_errors() {
        let (env, contract_addr) = setup();
        let admin_addr = Address::generate(&env);

        let id = env.as_contract(&contract_addr, || {
            PoolManager::set_timelock_admin(env.clone(), admin_addr).unwrap();
            PoolManager::queue_parameter_change(env.clone(), storage::MAX_UTIL, 9_000).unwrap()
        });

        env.as_contract(&contract_addr, || {
            PoolManager::cancel_parameter_change(env.clone(), id).unwrap();
        });

        advance_time(&env, 48 * 60 * 60);

        let err = env.as_contract(&contract_addr, || {
            PoolManager::execute_parameter_change(env.clone(), id)
        });
        assert_eq!(err, Err(ContractError::ActionCancelled));
    }

    #[test]
    fn execute_reclamps_financed_amount_when_ceiling_drops() {
        let (env, contract_addr) = setup();
        let admin_addr = Address::generate(&env);
        let alice = symbol_short!("alice");

        env.as_contract(&contract_addr, || {
            PoolManager::deposit(env.clone(), alice, 100_000).unwrap();
            PoolManager::finance(env.clone(), 75_000).unwrap(); // within the initial 80% ceiling
        });

        let id = env.as_contract(&contract_addr, || {
            PoolManager::set_timelock_admin(env.clone(), admin_addr).unwrap();
            PoolManager::queue_parameter_change(env.clone(), storage::MAX_UTIL, 5_000).unwrap()
        });

        advance_time(&env, 48 * 60 * 60);

        env.as_contract(&contract_addr, || {
            PoolManager::execute_parameter_change(env.clone(), id).unwrap();
            assert_eq!(PoolManager::max_utilisation(env.clone()), 5_000);
            assert_eq!(PoolManager::financed_amount(env.clone()), 50_000);
        });
    }

    #[test]
    fn queue_without_timelock_admin_set_errors() {
        let (env, contract_addr) = setup();
        let err = env.as_contract(&contract_addr, || {
            PoolManager::queue_parameter_change(env.clone(), storage::MAX_UTIL, 9_000)
        });
        assert_eq!(err, Err(ContractError::TimelockAdminNotSet));
    }

    #[test]
    fn set_timelock_admin_is_one_time() {
        let (env, contract_addr) = setup();
        let a = Address::generate(&env);
        let b = Address::generate(&env);
        let err = env.as_contract(&contract_addr, || {
            PoolManager::set_timelock_admin(env.clone(), a).unwrap();
            PoolManager::set_timelock_admin(env.clone(), b)
        });
        assert_eq!(err, Err(ContractError::TimelockAdminAlreadySet));
    }
}
