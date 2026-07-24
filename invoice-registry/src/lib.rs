#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, Symbol,
};

/// Errors surfaced by the registry. Stable `u32` discriminants so integrators
/// and audit tooling can match on them.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    /// `initialize` called on an already-initialized contract.
    AlreadyInitialized = 1,
    /// An admin-guarded call was made before `initialize`.
    NotInitialized = 2,
    /// `accept_admin` called with no transfer in progress.
    NoPendingAdmin = 3,
}

/// Storage keys for the registry's instance state.
#[contracttype]
#[derive(Clone)]
enum DataKey {
    /// The current administrator.
    Admin,
    /// The address that has been nominated to become admin but has not yet
    /// accepted (the second step of the two-step transfer).
    PendingAdmin,
}

/// Invoice lifecycle and verification.
#[contract]
pub struct InvoiceRegistry;

#[contractimpl]
impl InvoiceRegistry {
    /// One-time initialization. Sets the initial administrator.
    ///
    /// Returns [`Error::AlreadyInitialized`] if the contract has already been
    /// initialized, so the admin can never be silently overwritten.
    pub fn initialize(env: Env, admin: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        Ok(())
    }

    /// Step 1 of the two-step admin handover: the **current** admin nominates
    /// `new_admin`. Authorization from the current admin is required, and the
    /// nomination only takes effect once `new_admin` calls [`accept_admin`].
    ///
    /// Re-calling overwrites any outstanding nomination, so a mistaken transfer
    /// can be corrected before it is accepted.
    pub fn transfer_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        let admin = Self::read_admin(&env)?;
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &new_admin);
        Ok(())
    }

    /// Step 2 of the two-step admin handover: the **pending** admin accepts the
    /// nomination. Authorization from the pending admin is required, which
    /// guarantees the new admin controls a live, signing address (no transfer
    /// to a typo'd or unusable address can ever complete).
    ///
    /// On success the admin is replaced, the pending slot is cleared, and an
    /// `AdminTransferred` event is emitted.
    pub fn accept_admin(env: Env) -> Result<(), Error> {
        let pending: Address = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .ok_or(Error::NoPendingAdmin)?;
        pending.require_auth();

        let previous = Self::read_admin(&env)?;
        env.storage().instance().set(&DataKey::Admin, &pending);
        env.storage().instance().remove(&DataKey::PendingAdmin);

        env.events().publish(
            (symbol_short!("adm_xfer"), previous, pending.clone()),
            pending,
        );
        Ok(())
    }

    /// Returns the current administrator, or [`Error::NotInitialized`].
    pub fn get_admin(env: Env) -> Result<Address, Error> {
        Self::read_admin(&env)
    }

    /// Returns the pending administrator, if a transfer is in progress.
    pub fn get_pending_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::PendingAdmin)
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

    /// Internal helper: read the admin or return [`Error::NotInitialized`]. Kept
    /// private so the "must be initialized" invariant lives in exactly one place.
    fn read_admin(env: &Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
    }
}

#[cfg(test)]
mod test;

// Contribution check by nancy-k at 2024-11-21T23:51:43

// Contribution check by oluwagbemiga at 2025-02-26T05:22:45

// Contribution check by johndoedev at 2025-06-02T10:53:47

// Contribution check by nancy-k at 2025-09-06T16:24:49

// Contribution check by oluwagbemiga at 2025-12-11T21:55:51

// Contribution check by johndoedev at 2026-03-18T03:26:53

// Contribution by kulayddon — 2024-11-23

// Contribution by Gbangbolaoluwagbemiga — 2024-12-23

// Contribution by kulayddon — 2025-01-21

// Contribution by Gbangbolaoluwagbemiga — 2025-02-20

// Contribution by kulayddon — 2025-03-21

// Contribution by Gbangbolaoluwagbemiga — 2025-04-19

// Contribution by kulayddon — 2025-05-19

// Contribution by Gbangbolaoluwagbemiga — 2025-06-17

// Contribution by kulayddon — 2025-07-17

// Contribution by Gbangbolaoluwagbemiga — 2025-08-15

// Contribution by kulayddon — 2025-09-14

// Contribution by Gbangbolaoluwagbemiga — 2025-10-13

// Contribution by kulayddon — 2025-11-11

// Contribution by Gbangbolaoluwagbemiga — 2025-12-11

// Contribution by kulayddon — 2026-01-09

// Contribution by Gbangbolaoluwagbemiga — 2026-02-08

// Contribution by kulayddon — 2026-03-09

// Contribution by Gbangbolaoluwagbemiga — 2026-04-07

// Contribution by kulayddon — 2026-05-07

// Contribution by Gbangbolaoluwagbemiga — 2026-06-05
