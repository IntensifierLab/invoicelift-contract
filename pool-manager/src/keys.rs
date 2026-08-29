//! Named storage-key constants for the pool-manager contract.
//!
//! Every persistent/instance storage key lives here as a named `Symbol`
//! constant rather than an inline string literal at the call site — a typo
//! in an inline `symbol_short!("...")` silently creates a new, disconnected
//! storage slot instead of failing to compile.

use soroban_sdk::{symbol_short, Symbol};

/// Legacy `Symbol` admin tag. See `ADMIN_ADDR` for the authenticable one.
pub const ADMIN: Symbol = symbol_short!("admin");
/// Sum of all lender shares outstanding.
pub const TOTAL_SHARES: Symbol = symbol_short!("tot_sh");
/// Pool capital, in token units: `TOTAL_SHARES * NAV / NAV_SCALE`.
pub const TOTAL_CAPITAL: Symbol = symbol_short!("tot_ca");
/// Capital currently deployed against financed invoices.
pub const FINANCED_AMT: Symbol = symbol_short!("fin_am");
/// Utilisation ceiling, in bps, that `FINANCED_AMT` may not exceed.
pub const MAX_UTIL: Symbol = symbol_short!("max_ut");
/// Net asset value per share, scaled by `NAV_SCALE`.
pub const NAV: Symbol = symbol_short!("nav");
/// Governance flag: excludes this pool from being picked as a rebalancing donor.
pub const DONOR_BLK: Symbol = symbol_short!("dnr_blk");
/// Address authorized to queue/cancel timelocked admin actions. Separate
/// from the legacy `ADMIN` symbol tag: this is a real `Address` so it can
/// be authenticated with `require_auth`.
pub const ADMIN_ADDR: Symbol = symbol_short!("adm_addr");
/// Next id to assign to a queued timelocked action.
pub const NEXT_ACTION: Symbol = symbol_short!("nxt_act");

// ── Concentration limits (issue #19) ────────────────────────────────
/// Per-buyer concentration ceiling, in bps of pool capital.
pub const BUYER_CONC_BPS: Symbol = symbol_short!("byr_cnc");
/// Per-SME concentration ceiling, in bps of pool capital.
pub const SME_CONC_BPS: Symbol = symbol_short!("sme_cnc");
/// Marker field for `BuyerExposureKey` — see that type's doc comment for
/// why a marker is required.
pub const BUYER_EXP_TAG: Symbol = symbol_short!("byr_exp");
/// Marker field for `SmeExposureKey` — see that type's doc comment for
/// why a marker is required.
pub const SME_EXP_TAG: Symbol = symbol_short!("sme_exp");

// ── Pool creation config (issue #17) ────────────────────────────────
/// Whether `create_pool` has already been called (guards duplicate config).
pub const POOL_CFG_SET: Symbol = symbol_short!("pl_cfg");
/// Buyer exposure limit fixed at pool creation, in bps.
pub const BUYER_LIMIT_BPS: Symbol = symbol_short!("buy_lim");
/// SME exposure limit fixed at pool creation, in bps.
pub const SME_LIMIT_BPS: Symbol = symbol_short!("sme_lim");
/// Smallest accepted single deposit.
pub const MIN_DEPOSIT: Symbol = symbol_short!("min_dep");
/// Largest accepted single deposit.
pub const MAX_DEPOSIT: Symbol = symbol_short!("max_dep");
/// Share of capital, in bps, kept idle as reserve.
pub const RESERVE_RATIO_BPS: Symbol = symbol_short!("rsv_rat");

// ── Withdrawal lock period (issue #22) ──────────────────────────────
/// Seconds a deposit must sit before it can be withdrawn. Defaults to 0
/// (no lock) if never configured via `set_lock_period`.
pub const LOCK_SECS: Symbol = symbol_short!("lock_sec");
/// Withdrawals are blocked once pool utilisation (financed / capital)
/// exceeds this bps threshold. Defaults to `BPS_SCALE` (100%, i.e. no
/// extra restriction) if never configured.
pub const WITHDRAW_UTIL_BPS: Symbol = symbol_short!("wd_util");
/// Marker field for `DepositTimeKey` — see that type's doc comment for why.
pub const DEP_TS_TAG: Symbol = symbol_short!("dep_ts");

/// The currently-queued upgrade, if any.
pub const QUEUED_UPGRADE: Symbol = symbol_short!("q_upgrd");
/// Emergency-pause flag blocking `execute_upgrade` (queueing/cancelling
/// still work while paused).
pub const UPGRADE_PAUSED: Symbol = symbol_short!("up_pause");

#[cfg(test)]
mod tests {
    use super::*;

    /// Locks in the on-chain symbol for each key — an accidental rename here
    /// is a silent storage-slot migration, not a compile error.
    #[test]
    fn key_symbols_are_stable() {
        assert_eq!(ADMIN, symbol_short!("admin"));
        assert_eq!(ADMIN_ADDR, symbol_short!("adm_addr"));
        assert_eq!(TOTAL_SHARES, symbol_short!("tot_sh"));
        assert_eq!(NAV, symbol_short!("nav"));
        assert_eq!(QUEUED_UPGRADE, symbol_short!("q_upgrd"));
        assert_eq!(UPGRADE_PAUSED, symbol_short!("up_pause"));
    }
}
