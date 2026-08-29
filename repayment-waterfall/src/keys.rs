//! Named storage-key constants for the repayment-waterfall contract.
//!
//! Every persistent/instance storage key lives here as a named `Symbol`
//! constant rather than an inline string literal at the call site — a typo
//! in an inline `symbol_short!("...")` silently creates a new, disconnected
//! storage slot instead of failing to compile.

use soroban_sdk::{symbol_short, Symbol};

/// Instance-storage tag holding the contract admin.
pub const ADMIN: Symbol = symbol_short!("admin");
/// Address of the `pool-manager` contract this waterfall forwards
/// repayments to and reports defaults against.
pub const POOL_MANAGER: Symbol = symbol_short!("pool_mgr");

/// Grace period (seconds) applied before an overdue invoice can be
/// declared in default. Defaults to 0 (no grace) if never configured via
/// `set_grace_period`.
pub const GRACE_SECS: Symbol = symbol_short!("grace");
/// Marker fields for `InvoiceDueKey`/`InvoiceDefaultedKey` — see those
/// types' doc comments for why a marker is required.
pub const DUE_TAG: Symbol = symbol_short!("due_tag");
pub const DEFAULT_TAG: Symbol = symbol_short!("dflt_tag");

/// Storage key for the current rolling-window `DefaultRecord`.
pub const DEFAULT_RECORD: Symbol = symbol_short!("dflt_rec");
/// Storage key for the configured circuit-breaker default-volume threshold.
pub const CB_THRESHOLD: Symbol = symbol_short!("cb_thresh");
/// Storage key for the circuit-breaker tripped flag.
pub const CB_ACTIVE: Symbol = symbol_short!("cb_active");

/// The currently-queued upgrade, if any.
pub const QUEUED_UPGRADE: Symbol = symbol_short!("q_upgrd");
/// Emergency-pause flag blocking `execute_upgrade` (queueing/cancelling
/// still work while paused).
pub const UPGRADE_PAUSED: Symbol = symbol_short!("up_pause");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_symbols_are_stable() {
        assert_eq!(ADMIN, symbol_short!("admin"));
        assert_eq!(POOL_MANAGER, symbol_short!("pool_mgr"));
        assert_eq!(GRACE_SECS, symbol_short!("grace"));
        assert_eq!(DUE_TAG, symbol_short!("due_tag"));
        assert_eq!(DEFAULT_TAG, symbol_short!("dflt_tag"));
        assert_eq!(DEFAULT_RECORD, symbol_short!("dflt_rec"));
        assert_eq!(CB_THRESHOLD, symbol_short!("cb_thresh"));
        assert_eq!(CB_ACTIVE, symbol_short!("cb_active"));
        assert_eq!(QUEUED_UPGRADE, symbol_short!("q_upgrd"));
        assert_eq!(UPGRADE_PAUSED, symbol_short!("up_pause"));
    }
}
