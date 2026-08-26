//! Named storage-key constants for the invoice-registry contract.
//!
//! Every persistent/instance storage key lives here as a named `Symbol`
//! constant rather than an inline string literal at the call site.

use soroban_sdk::{symbol_short, Symbol};

/// Instance-storage tag holding the registry admin.
pub const ADMIN: Symbol = symbol_short!("admin");
/// Marker field for `VerifierKey` — see that type's doc comment for why.
pub const VERIFIER_TAG: Symbol = symbol_short!("verifier");
/// Marker field for `TermsKey` — same collision reasoning as `VerifierKey`.
pub const TERMS_TAG: Symbol = symbol_short!("terms");

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
        assert_eq!(VERIFIER_TAG, symbol_short!("verifier"));
        assert_eq!(TERMS_TAG, symbol_short!("terms"));
        assert_eq!(QUEUED_UPGRADE, symbol_short!("q_upgrd"));
        assert_eq!(UPGRADE_PAUSED, symbol_short!("up_pause"));
    }
}
