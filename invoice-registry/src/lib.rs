#![no_std]

//! # Invoice Registry — Confidential Invoice Amounts
//!
//! Invoice amounts are stored as Pedersen commitments — no plaintext values
//! are ever written on-chain. Only parties holding the opening `(value, blinding)`
//! can verify (or prove to an auditor) the original amount.

mod nft;
mod pedersen;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, Symbol,
};

pub use nft::TransferRecord;

// ─── Storage Keys ──────────────────────────────────────────────────────────

mod storage {
    use soroban_sdk::{symbol_short, Symbol};

    pub const ADMIN: Symbol = symbol_short!("admin");
    /// Marker field for `VerifierKey` — see that type's doc comment for why.
    pub const VERIFIER_TAG: Symbol = symbol_short!("verifier");
}

/// Errors surfaced by the invoice registry. Stable `u32` discriminants so
/// integrators and audit tooling can match on them.
#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ContractError {
    /// `initialize` called on an already-initialized contract.
    AlreadyInitialized = 1,
    /// An admin-guarded call was made before `initialize`.
    NotInitialized = 2,
    /// `register` was called with an id that already exists.
    InvoiceAlreadyExists = 3,
    /// The referenced invoice id does not exist.
    InvoiceNotFound = 4,
    /// The caller is not the admin.
    Unauthorized = 5,
    /// A lifecycle transition was attempted from the wrong status.
    InvalidStatus = 6,
    /// A commitment scale could not be inverted modulo P (not coprime).
    ScaleNotInvertible = 7,
    /// A transition was attempted on an invoice currently frozen.
    InvoiceFrozen = 8,
    /// `verify_invoice` called by an address not granted verifier status.
    NotVerifier = 9,
    /// `execute_upgrade`/`cancel_upgrade` called with nothing queued.
    NoQueuedUpgrade = 10,
    /// `execute_upgrade` was called before the timelock elapsed.
    UpgradeTimelockNotElapsed = 11,
    /// `execute_upgrade` was called while upgrades are emergency-paused.
    UpgradesPaused = 12,
}

/// Per-invoice storage key.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvoiceKey(pub Symbol);

/// Per-verifier storage key (issue #14): `VerifierKey(marker, verifier) -> bool`.
///
/// Carries a fixed marker as its first field: `#[contracttype]` encodes a
/// tuple struct as a bare XDR vec of its fields with no type-name
/// discriminant, so a single-`Symbol`-field key here would be
/// indistinguishable on-ledger from `InvoiceKey` (or any other
/// single-`Symbol` key) sharing the same value — the marker keeps this
/// key's storage slot from colliding with theirs.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifierKey(pub Symbol, pub Symbol);

// ─── Types ─────────────────────────────────────────────────────────────────

/// Invoice lifecycle status.
#[contracttype]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum InvoiceStatus {
    /// Registered by SME, awaiting approval.
    Pending = 0,
    /// Approved by admin / verifier.
    Approved = 1,
    /// Assigned to a pool for financing.
    Assigned = 2,
    /// Buyer has repaid; invoice settled.
    Repaid = 3,
}

/// On-chain invoice record. The `commitment` field is a Pedersen commitment
/// of the invoice amount — the plaintext is never stored.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommittedInvoice {
    /// Unique invoice identifier.
    pub id: Symbol,
    /// Pedersen commitment of the invoice amount: `C(value, blinding) mod p`.
    pub commitment: i128,
    /// Current lifecycle status.
    pub status: InvoiceStatus,
    /// Current owner (SME initially, pool after assignment).
    pub owner: Symbol,
    /// Set by admin via `flag_invoice` (issue #16) when the invoice is
    /// suspected fraudulent. Informational — does not itself block
    /// transitions; see `frozen`.
    pub fraud_flagged: bool,
    /// Set by admin via `freeze_invoice` (issue #16). While `true`, all
    /// state-transition entrypoints (`approve`, `assign`, `mark_repaid`)
    /// return `ContractError::InvoiceFrozen`.
    pub frozen: bool,
}

// ─── Contract ──────────────────────────────────────────────────────────────

/// Invoice lifecycle and verification.
#[contract]
pub struct InvoiceRegistry;

#[contractimpl]
impl InvoiceRegistry {
    /// One-time initialization. Sets the initial administrator.
    ///
    /// Returns [`ContractError::AlreadyInitialized`] if the contract has
    /// already been initialized, so the admin can never be silently
    /// overwritten.
    pub fn initialize(env: Env, admin: Symbol) -> Result<(), ContractError> {
        if env.storage().instance().has(&storage::ADMIN) {
            return Err(ContractError::AlreadyInitialized);
        }
        env.storage().instance().set(&storage::ADMIN, &admin);
        Ok(())
    }

    // ── Invoice lifecycle ────────────────────────────────────────────────

    /// Register a new invoice with a Pedersen commitment of the amount.
    ///
    /// The SME computes `commitment = pedersen::commit(amount, blinding)` off-chain
    /// and submits only the commitment. The plaintext amount and blinding factor
    /// remain secret.
    pub fn register(
        env: Env,
        id: Symbol,
        commitment: i128,
        owner: Symbol,
    ) -> Result<(), ContractError> {
        let key = InvoiceKey(id.clone());
        if env.storage().persistent().has(&key) {
            return Err(ContractError::InvoiceAlreadyExists);
        }

        let invoice = CommittedInvoice {
            id,
            commitment,
            status: InvoiceStatus::Pending,
            owner,
            fraud_flagged: false,
            frozen: false,
        };

        env.storage().persistent().set(&key, &invoice);
        Ok(())
    }

    /// Admin approves a pending invoice (Pending → Approved).
    pub fn approve(env: Env, caller: Symbol, id: Symbol) -> Result<(), ContractError> {
        Self::require_admin(&env, &caller)?;

        let key = InvoiceKey(id);
        let mut invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvoiceNotFound)?;

        if invoice.frozen {
            return Err(ContractError::InvoiceFrozen);
        }
        if invoice.status != InvoiceStatus::Pending {
            return Err(ContractError::InvalidStatus);
        }

        invoice.status = InvoiceStatus::Approved;
        env.storage().persistent().set(&key, &invoice);
        Ok(())
    }

    // ── Verification (issue #14) ────────────────────────────────────────

    /// Admin grants (or revokes) verifier status for `verifier`. Only
    /// addresses granted verifier status may call [`Self::verify_invoice`].
    pub fn set_verifier(
        env: Env,
        caller: Symbol,
        verifier: Symbol,
        is_verifier: bool,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env, &caller)?;
        env.storage()
            .persistent()
            .set(&VerifierKey(storage::VERIFIER_TAG, verifier), &is_verifier);
        Ok(())
    }

    /// Whether `verifier` currently holds verifier status.
    pub fn is_verifier(env: Env, verifier: Symbol) -> bool {
        env.storage()
            .persistent()
            .get(&VerifierKey(storage::VERIFIER_TAG, verifier))
            .unwrap_or(false)
    }

    /// A verifier (granted via [`Self::set_verifier`]) verifies a pending
    /// invoice (Pending → Approved). This is a separate entrypoint from
    /// [`Self::approve`] so verification duties can be delegated to a set of
    /// verifiers distinct from the contract admin.
    ///
    /// Returns [`ContractError::NotVerifier`] if the caller was never
    /// granted verifier status, or [`ContractError::InvalidStatus`] if the
    /// invoice is not currently `Pending`.
    pub fn verify_invoice(env: Env, caller: Symbol, id: Symbol) -> Result<(), ContractError> {
        if !Self::is_verifier(env.clone(), caller) {
            return Err(ContractError::NotVerifier);
        }

        let key = InvoiceKey(id);
        let mut invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvoiceNotFound)?;

        if invoice.status != InvoiceStatus::Pending {
            return Err(ContractError::InvalidStatus);
        }

        invoice.status = InvoiceStatus::Approved;
        env.storage().persistent().set(&key, &invoice);

        env.events()
            .publish((symbol_short!("inv_ver"),), invoice.id);
        Ok(())
    }

    /// Admin assigns an approved invoice to a new owner / pool (Approved → Assigned).
    pub fn assign(
        env: Env,
        caller: Symbol,
        id: Symbol,
        new_owner: Symbol,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env, &caller)?;

        let key = InvoiceKey(id);
        let mut invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvoiceNotFound)?;

        if invoice.frozen {
            return Err(ContractError::InvoiceFrozen);
        }
        if invoice.status != InvoiceStatus::Approved {
            return Err(ContractError::InvalidStatus);
        }

        invoice.status = InvoiceStatus::Assigned;
        invoice.owner = new_owner;
        env.storage().persistent().set(&key, &invoice);
        Ok(())
    }

    // ── Pool financing assignment (issue #15) ───────────────────────────

    /// Transfers financing rights on an approved invoice to a pool, callable
    /// only by the pool-manager contract identified by `pool_manager`
    /// (authenticated via `require_auth`, not the registry admin).
    ///
    /// Transitions Approved → Assigned and stores `pool` (the financing pool
    /// identifier) as the invoice's owner. Emits an `InvoiceAssigned` event
    /// carrying the pool address.
    pub fn assign_invoice(
        env: Env,
        pool_manager: Address,
        id: Symbol,
        pool: Symbol,
    ) -> Result<(), ContractError> {
        pool_manager.require_auth();

        let key = InvoiceKey(id);
        let mut invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvoiceNotFound)?;

        if invoice.status != InvoiceStatus::Approved {
            return Err(ContractError::InvalidStatus);
        }

        invoice.status = InvoiceStatus::Assigned;
        invoice.owner = pool.clone();
        env.storage().persistent().set(&key, &invoice);

        env.events()
            .publish((symbol_short!("inv_asgn"), pool), invoice.id);
        Ok(())
    }

    /// Admin marks an assigned invoice as repaid (Assigned → Repaid).
    pub fn mark_repaid(env: Env, caller: Symbol, id: Symbol) -> Result<(), ContractError> {
        Self::require_admin(&env, &caller)?;

        let key = InvoiceKey(id);
        let mut invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvoiceNotFound)?;

        if invoice.frozen {
            return Err(ContractError::InvoiceFrozen);
        }
        if invoice.status != InvoiceStatus::Assigned {
            return Err(ContractError::InvalidStatus);
        }

        invoice.status = InvoiceStatus::Repaid;
        env.storage().persistent().set(&key, &invoice);
        Ok(())
    }

    // ── Fraud flag & freeze (issue #16) ─────────────────────────────────

    /// Admin marks an invoice as suspicious. Informational only — does not
    /// block transitions on its own; pair with [`Self::freeze_invoice`] to
    /// actually halt the invoice.
    pub fn flag_invoice(env: Env, caller: Symbol, id: Symbol) -> Result<(), ContractError> {
        Self::require_admin(&env, &caller)?;

        let key = InvoiceKey(id);
        let mut invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvoiceNotFound)?;

        invoice.fraud_flagged = true;
        env.storage().persistent().set(&key, &invoice);

        env.events()
            .publish((symbol_short!("inv_flag"),), invoice.id);
        Ok(())
    }

    /// Admin freezes an invoice, blocking all further state transitions
    /// (`approve`, `assign`, `mark_repaid`) until lifted.
    pub fn freeze_invoice(env: Env, caller: Symbol, id: Symbol) -> Result<(), ContractError> {
        Self::require_admin(&env, &caller)?;

        let key = InvoiceKey(id);
        let mut invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvoiceNotFound)?;

        invoice.frozen = true;
        env.storage().persistent().set(&key, &invoice);

        env.events()
            .publish((symbol_short!("inv_frz"),), invoice.id);
        Ok(())
    }

    /// Admin lifts a freeze, with `justification` recorded in the emitted
    /// event for the audit trail.
    pub fn unfreeze_invoice(
        env: Env,
        caller: Symbol,
        id: Symbol,
        justification: Symbol,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env, &caller)?;

        let key = InvoiceKey(id);
        let mut invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ContractError::InvoiceNotFound)?;

        invoice.frozen = false;
        env.storage().persistent().set(&key, &invoice);

        env.events()
            .publish((symbol_short!("inv_unfrz"), justification), invoice.id);
        Ok(())
    }

    // ── Queries ──────────────────────────────────────────────────────────

    /// Retrieve an invoice record (commitment only — no plaintext amount).
    pub fn get_invoice(env: Env, id: Symbol) -> Option<CommittedInvoice> {
        env.storage().persistent().get(&InvoiceKey(id))
    }

    /// Verify that a claimed `(value, blinding)` matches the stored commitment
    /// for the given invoice. Returns `true` if `commit(value, blinding) == stored_commitment`.
    ///
    /// This allows an SME to reveal their amount to an auditor/buyer without
    /// ever storing the plaintext on-chain.
    pub fn verify_amount(
        env: Env,
        id: Symbol,
        value: i128,
        blinding: i128,
    ) -> Result<bool, ContractError> {
        let invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&InvoiceKey(id))
            .ok_or(ContractError::InvoiceNotFound)?;

        Ok(pedersen::verify(invoice.commitment, value, blinding))
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

    // ── Internal helpers ─────────────────────────────────────────────────

    fn require_admin(env: &Env, caller: &Symbol) -> Result<(), ContractError> {
        let admin: Symbol = env
            .storage()
            .instance()
            .get(&storage::ADMIN)
            .ok_or(ContractError::NotInitialized)?;
        if *caller != admin {
            return Err(ContractError::Unauthorized);
        }
        Ok(())
    }
}

// ── Invoice NFT tokenisation ────────────────────────────────────────────
//
// A separate `impl` block (rather than more methods on the block above) so
// this addition doesn't need to touch the existing entrypoints' region at
// all - see `nft` for the full design (token model, royalty-hook,
// single-owner repayment-share simplification).
#[contractimpl]
impl InvoiceRegistry {
    /// Mint the token for `token_id` (typically an invoice id, called once
    /// it's financed) with `owner` as its initial owner. Admin-gated.
    pub fn mint_invoice_token(
        env: Env,
        caller: Symbol,
        token_id: Symbol,
        owner: Symbol,
    ) -> Result<(), ContractError> {
        Self::require_admin(&env, &caller)?;
        nft::mint(&env, token_id, owner);
        Ok(())
    }

    /// Transfer `token_id` from `from` to `to`, recording the transfer
    /// on-chain and publishing a royalty-hook event. `from` must be the
    /// token's current owner.
    pub fn transfer_invoice_token(
        env: Env,
        token_id: Symbol,
        from: Symbol,
        to: Symbol,
        royalty_bps: u32,
    ) {
        nft::transfer(&env, token_id, from, to, royalty_bps);
    }

    /// Current owner of `token_id`, or `None` if it hasn't been minted.
    pub fn invoice_owner(env: Env, token_id: Symbol) -> Option<Symbol> {
        nft::owner_of(&env, token_id)
    }

    /// Full on-chain transfer history for `token_id`.
    pub fn invoice_transfer_history(env: Env, token_id: Symbol) -> soroban_sdk::Vec<TransferRecord> {
        nft::transfer_history(&env, token_id)
    }

    /// The current owner's share of `total_repayment` (single-owner model —
    /// see `nft` module docs).
    pub fn invoice_repayment_share(
        env: Env,
        token_id: Symbol,
        total_repayment: i128,
    ) -> (Symbol, i128) {
        nft::repayment_share(&env, token_id, total_repayment)
    }
}

/// A queued, timelocked contract-Wasm upgrade.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedUpgrade {
    pub new_wasm_hash: soroban_sdk::BytesN<32>,
    pub execute_after: u64,
}

const UPGRADE_TIMELOCK_SECS: u64 = 48 * 60 * 60;
const QUEUED_UPGRADE: Symbol = symbol_short!("q_upgrd");
const UPGRADE_PAUSED: Symbol = symbol_short!("up_pause");

// ── upgrade (proxy upgradability pattern) ───────────────────────────────
//
// Separate impl block (see issue #32's PRs on the sibling contracts for the
// full design rationale): Soroban upgrades in place via Wasm-hash swap,
// preserving storage automatically, so that IS this platform's
// proxy-upgrade equivalent - no separate proxy contract needed.
#[contractimpl]
impl InvoiceRegistry {
    /// Queues an upgrade to `new_wasm_hash`, executable no earlier than 48h
    /// from now. Requires the stored admin. Returns the ledger timestamp
    /// after which it becomes executable.
    pub fn queue_upgrade(
        env: Env,
        caller: Symbol,
        new_wasm_hash: soroban_sdk::BytesN<32>,
    ) -> Result<u64, ContractError> {
        Self::require_admin(&env, &caller)?;

        let execute_after = env.ledger().timestamp() + UPGRADE_TIMELOCK_SECS;
        env.storage().instance().set(
            &QUEUED_UPGRADE,
            &QueuedUpgrade {
                new_wasm_hash,
                execute_after,
            },
        );
        Ok(execute_after)
    }

    /// Executes the queued upgrade once its timelock has elapsed. Callable
    /// by anyone.
    pub fn execute_upgrade(env: Env) -> Result<(), ContractError> {
        let paused: bool = env.storage().instance().get(&UPGRADE_PAUSED).unwrap_or(false);
        if paused {
            return Err(ContractError::UpgradesPaused);
        }

        let queued: QueuedUpgrade = env
            .storage()
            .instance()
            .get(&QUEUED_UPGRADE)
            .ok_or(ContractError::NoQueuedUpgrade)?;
        if env.ledger().timestamp() < queued.execute_after {
            return Err(ContractError::UpgradeTimelockNotElapsed);
        }

        env.storage().instance().remove(&QUEUED_UPGRADE);
        env.deployer().update_current_contract_wasm(queued.new_wasm_hash);
        Ok(())
    }

    /// Cancels the queued upgrade before it executes. Requires the stored
    /// admin.
    pub fn cancel_upgrade(env: Env, caller: Symbol) -> Result<(), ContractError> {
        Self::require_admin(&env, &caller)?;
        if !env.storage().instance().has(&QUEUED_UPGRADE) {
            return Err(ContractError::NoQueuedUpgrade);
        }
        env.storage().instance().remove(&QUEUED_UPGRADE);
        Ok(())
    }

    /// Sets the emergency-pause flag blocking `execute_upgrade`. Requires
    /// the stored admin.
    pub fn set_upgrade_paused(env: Env, caller: Symbol, paused: bool) -> Result<(), ContractError> {
        Self::require_admin(&env, &caller)?;
        env.storage().instance().set(&UPGRADE_PAUSED, &paused);
        Ok(())
    }

    pub fn queued_upgrade(env: Env) -> Option<QueuedUpgrade> {
        env.storage().instance().get(&QUEUED_UPGRADE)
    }

    pub fn is_upgrade_paused(env: Env) -> bool {
        env.storage().instance().get(&UPGRADE_PAUSED).unwrap_or(false)
    }
}

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

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::testutils::Ledger as _;
    use soroban_sdk::{Address, Env};

    fn setup() -> (Env, soroban_sdk::Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_addr = env.register_contract(None::<&Address>, InvoiceRegistry);
        env.as_contract(&contract_addr, || {
            InvoiceRegistry::initialize(env.clone(), symbol_short!("admin")).unwrap();
        });
        (env, contract_addr)
    }

    fn test_commitment(value: i128, blinding: i128) -> i128 {
        pedersen::commit(value, blinding)
    }

    // ── Registration ─────────────────────────────────────────────────────

    #[test]
    fn test_register_invoice_stores_commitment() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV001");
        let commitment = test_commitment(50_000, 12345);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
        });

        let invoice = env.as_contract(&contract_addr, || {
            InvoiceRegistry::get_invoice(env.clone(), inv_id).expect("invoice missing")
        });

        assert_eq!(invoice.commitment, commitment);
        assert_eq!(invoice.status, InvoiceStatus::Pending);
        assert_eq!(invoice.owner, symbol_short!("sme1"));
    }

    #[test]
    fn test_register_duplicate_errors() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV002");
        let commitment = test_commitment(10_000, 999);

        let err = env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
        });
        assert_eq!(err, Err(ContractError::InvoiceAlreadyExists));
    }

    // ── Approval ─────────────────────────────────────────────────────────

    #[test]
    fn test_approve_transitions_status() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV003");
        let commitment = test_commitment(20_000, 555);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            InvoiceRegistry::approve(env.clone(), symbol_short!("admin"), inv_id.clone()).unwrap();
        });

        let invoice = env.as_contract(&contract_addr, || {
            InvoiceRegistry::get_invoice(env.clone(), inv_id).expect("missing")
        });
        assert_eq!(invoice.status, InvoiceStatus::Approved);
    }

    #[test]
    fn test_approve_non_admin_errors() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV004");
        let commitment = test_commitment(30_000, 777);

        let err = env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            InvoiceRegistry::approve(env.clone(), symbol_short!("hacker"), inv_id)
        });
        assert_eq!(err, Err(ContractError::Unauthorized));
    }

    // ── Verification ─────────────────────────────────────────────────────

    #[test]
    fn test_verify_invoice_by_granted_verifier_transitions_status() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV010");
        let commitment = test_commitment(15_000, 111);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            InvoiceRegistry::set_verifier(
                env.clone(),
                symbol_short!("admin"),
                symbol_short!("ver1"),
                true,
            )
            .unwrap();
            InvoiceRegistry::verify_invoice(env.clone(), symbol_short!("ver1"), inv_id.clone())
                .unwrap();
        });

        let invoice = env.as_contract(&contract_addr, || {
            InvoiceRegistry::get_invoice(env.clone(), inv_id).expect("missing")
        });
        assert_eq!(invoice.status, InvoiceStatus::Approved);
    }

    #[test]
    fn test_verify_invoice_non_verifier_errors() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV011");
        let commitment = test_commitment(15_000, 222);

        let err = env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            InvoiceRegistry::verify_invoice(env.clone(), symbol_short!("rando"), inv_id)
        });
        assert_eq!(err, Err(ContractError::NotVerifier));
    }

    #[test]
    fn test_verify_invoice_wrong_state_errors() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV012");
        let commitment = test_commitment(15_000, 333);

        let err = env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            InvoiceRegistry::set_verifier(
                env.clone(),
                symbol_short!("admin"),
                symbol_short!("ver1"),
                true,
            )
            .unwrap();
            InvoiceRegistry::approve(env.clone(), symbol_short!("admin"), inv_id.clone())
                .unwrap();
            // Already Approved — not Pending — so this must error.
            InvoiceRegistry::verify_invoice(env.clone(), symbol_short!("ver1"), inv_id)
        });
        assert_eq!(err, Err(ContractError::InvalidStatus));
    }

    // ── Assignment ───────────────────────────────────────────────────────

    #[test]
    fn test_assign_transitions_owner() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV005");
        let commitment = test_commitment(40_000, 888);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            InvoiceRegistry::approve(env.clone(), symbol_short!("admin"), inv_id.clone()).unwrap();
            InvoiceRegistry::assign(
                env.clone(),
                symbol_short!("admin"),
                inv_id.clone(),
                symbol_short!("pool1"),
            )
            .unwrap();
        });

        let invoice = env.as_contract(&contract_addr, || {
            InvoiceRegistry::get_invoice(env.clone(), inv_id).expect("missing")
        });
        assert_eq!(invoice.status, InvoiceStatus::Assigned);
        assert_eq!(invoice.owner, symbol_short!("pool1"));
    }

    #[test]
    fn test_assign_invoice_by_pool_manager_stores_pool_address() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV013");
        let commitment = test_commitment(45_000, 444);
        let pool_manager = Address::generate(&env);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            InvoiceRegistry::approve(env.clone(), symbol_short!("admin"), inv_id.clone()).unwrap();
            InvoiceRegistry::assign_invoice(
                env.clone(),
                pool_manager,
                inv_id.clone(),
                symbol_short!("pool9"),
            )
            .unwrap();
        });

        let invoice = env.as_contract(&contract_addr, || {
            InvoiceRegistry::get_invoice(env.clone(), inv_id).expect("missing")
        });
        assert_eq!(invoice.status, InvoiceStatus::Assigned);
        assert_eq!(invoice.owner, symbol_short!("pool9"));
    }

    #[test]
    fn test_assign_invoice_wrong_state_errors() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV014");
        let commitment = test_commitment(45_000, 555);
        let pool_manager = Address::generate(&env);

        let err = env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            // Still Pending — not Approved — so this must error.
            InvoiceRegistry::assign_invoice(
                env.clone(),
                pool_manager,
                inv_id,
                symbol_short!("pool9"),
            )
        });
        assert_eq!(err, Err(ContractError::InvalidStatus));
    }

    // ── Mark Repaid ──────────────────────────────────────────────────────

    #[test]
    fn test_mark_repaid_transitions_status() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV006");
        let commitment = test_commitment(60_000, 321);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            InvoiceRegistry::approve(env.clone(), symbol_short!("admin"), inv_id.clone()).unwrap();
            InvoiceRegistry::assign(
                env.clone(),
                symbol_short!("admin"),
                inv_id.clone(),
                symbol_short!("pool1"),
            )
            .unwrap();
            InvoiceRegistry::mark_repaid(env.clone(), symbol_short!("admin"), inv_id.clone())
                .unwrap();
        });

        let invoice = env.as_contract(&contract_addr, || {
            InvoiceRegistry::get_invoice(env.clone(), inv_id).expect("missing")
        });
        assert_eq!(invoice.status, InvoiceStatus::Repaid);
    }

    // ── Fraud flag & freeze ─────────────────────────────────────────────

    #[test]
    fn test_flag_invoice_sets_fraud_flag() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV015");
        let commitment = test_commitment(10_000, 1);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            InvoiceRegistry::flag_invoice(env.clone(), symbol_short!("admin"), inv_id.clone())
                .unwrap();
        });

        let invoice = env.as_contract(&contract_addr, || {
            InvoiceRegistry::get_invoice(env.clone(), inv_id).expect("missing")
        });
        assert!(invoice.fraud_flagged);
    }

    #[test]
    fn test_freeze_invoice_blocks_approve() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV016");
        let commitment = test_commitment(10_000, 2);

        let err = env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            InvoiceRegistry::freeze_invoice(env.clone(), symbol_short!("admin"), inv_id.clone())
                .unwrap();
            InvoiceRegistry::approve(env.clone(), symbol_short!("admin"), inv_id)
        });
        assert_eq!(err, Err(ContractError::InvoiceFrozen));
    }

    #[test]
    fn test_unfreeze_invoice_lifts_block() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV017");
        let commitment = test_commitment(10_000, 3);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            InvoiceRegistry::freeze_invoice(env.clone(), symbol_short!("admin"), inv_id.clone())
                .unwrap();
            InvoiceRegistry::unfreeze_invoice(
                env.clone(),
                symbol_short!("admin"),
                inv_id.clone(),
                symbol_short!("cleared"),
            )
            .unwrap();
            InvoiceRegistry::approve(env.clone(), symbol_short!("admin"), inv_id.clone()).unwrap();
        });

        let invoice = env.as_contract(&contract_addr, || {
            InvoiceRegistry::get_invoice(env.clone(), inv_id).expect("missing")
        });
        assert_eq!(invoice.status, InvoiceStatus::Approved);
        assert!(!invoice.frozen);
    }

    // ── Verify Amount ────────────────────────────────────────────────────

    #[test]
    fn test_verify_amount_correct_opening() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV007");
        let value: i128 = 75_000;
        let blinding: i128 = 13579;
        let commitment = test_commitment(value, blinding);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
        });

        let verified = env.as_contract(&contract_addr, || {
            InvoiceRegistry::verify_amount(env.clone(), inv_id, value, blinding).unwrap()
        });
        assert!(verified);
    }

    #[test]
    fn test_verify_amount_wrong_opening_fails() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV008");
        let commitment = test_commitment(75_000, 13579);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
        });

        let verified = env.as_contract(&contract_addr, || {
            InvoiceRegistry::verify_amount(env.clone(), inv_id, 99_999, 13579).unwrap()
        });
        assert!(!verified);
    }

    // ── Edge cases ───────────────────────────────────────────────────────

    #[test]
    fn test_get_invoice_non_existent_returns_none() {
        let (env, contract_addr) = setup();
        let result = env.as_contract(&contract_addr, || {
            InvoiceRegistry::get_invoice(env.clone(), symbol_short!("GHOST"))
        });
        assert!(result.is_none());
    }

    #[test]
    fn test_version_returns_two() {
        let (env, contract_addr) = setup();
        let v = env.as_contract(&contract_addr, || InvoiceRegistry::version(env.clone()));
        assert_eq!(v, 2);
    }

    #[test]
    fn test_full_lifecycle() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV009");
        let value: i128 = 100_000;
        let blinding: i128 = 24680;
        let commitment = test_commitment(value, blinding);

        env.as_contract(&contract_addr, || {
            // Register
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            )
            .unwrap();
            assert_eq!(
                InvoiceRegistry::get_invoice(env.clone(), inv_id.clone())
                    .unwrap()
                    .status,
                InvoiceStatus::Pending
            );

            // Approve
            InvoiceRegistry::approve(env.clone(), symbol_short!("admin"), inv_id.clone()).unwrap();
            assert_eq!(
                InvoiceRegistry::get_invoice(env.clone(), inv_id.clone())
                    .unwrap()
                    .status,
                InvoiceStatus::Approved
            );

            // Assign
            InvoiceRegistry::assign(
                env.clone(),
                symbol_short!("admin"),
                inv_id.clone(),
                symbol_short!("pool1"),
            )
            .unwrap();
            assert_eq!(
                InvoiceRegistry::get_invoice(env.clone(), inv_id.clone())
                    .unwrap()
                    .status,
                InvoiceStatus::Assigned
            );

            // Mark repaid
            InvoiceRegistry::mark_repaid(env.clone(), symbol_short!("admin"), inv_id.clone())
                .unwrap();
            assert_eq!(
                InvoiceRegistry::get_invoice(env.clone(), inv_id.clone())
                    .unwrap()
                    .status,
                InvoiceStatus::Repaid
            );

            // Verify amount at any stage
            assert!(InvoiceRegistry::verify_amount(env.clone(), inv_id, value, blinding).unwrap());
        });
    }

    // ── upgrade (proxy upgradability pattern) ───────────────────────────

    fn dummy_wasm_hash(env: &Env) -> soroban_sdk::BytesN<32> {
        soroban_sdk::BytesN::from_array(env, &[7u8; 32])
    }

    #[test]
    fn queue_upgrade_requires_admin() {
        let (env, contract_addr) = setup();
        let hash = dummy_wasm_hash(&env);
        let err = env.as_contract(&contract_addr, || {
            InvoiceRegistry::queue_upgrade(env.clone(), symbol_short!("hacker"), hash)
        });
        assert_eq!(err, Err(ContractError::Unauthorized));
    }

    #[test]
    fn queue_upgrade_sets_execute_after_48h_out() {
        let (env, contract_addr) = setup();
        let hash = dummy_wasm_hash(&env);

        let (queued_at, execute_after) = env.as_contract(&contract_addr, || {
            let now = env.ledger().timestamp();
            let execute_after =
                InvoiceRegistry::queue_upgrade(env.clone(), symbol_short!("admin"), hash).unwrap();
            (now, execute_after)
        });
        assert_eq!(execute_after, queued_at + 48 * 60 * 60);
    }

    #[test]
    fn execute_upgrade_before_timelock_elapses_errors() {
        let (env, contract_addr) = setup();
        let hash = dummy_wasm_hash(&env);
        let err = env.as_contract(&contract_addr, || {
            InvoiceRegistry::queue_upgrade(env.clone(), symbol_short!("admin"), hash).unwrap();
            InvoiceRegistry::execute_upgrade(env.clone())
        });
        assert_eq!(err, Err(ContractError::UpgradeTimelockNotElapsed));
    }

    #[test]
    fn execute_upgrade_with_nothing_queued_errors() {
        let (env, contract_addr) = setup();
        let err =
            env.as_contract(&contract_addr, || InvoiceRegistry::execute_upgrade(env.clone()));
        assert_eq!(err, Err(ContractError::NoQueuedUpgrade));
    }

    #[test]
    fn cancel_upgrade_clears_the_queue() {
        let (env, contract_addr) = setup();
        let hash = dummy_wasm_hash(&env);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::queue_upgrade(env.clone(), symbol_short!("admin"), hash).unwrap();
            InvoiceRegistry::cancel_upgrade(env.clone(), symbol_short!("admin")).unwrap();
            assert!(InvoiceRegistry::queued_upgrade(env.clone()).is_none());
        });
    }

    #[test]
    fn execute_upgrade_while_paused_errors_even_after_timelock_elapses() {
        let (env, contract_addr) = setup();
        let hash = dummy_wasm_hash(&env);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::queue_upgrade(env.clone(), symbol_short!("admin"), hash).unwrap();
            InvoiceRegistry::set_upgrade_paused(env.clone(), symbol_short!("admin"), true)
                .unwrap();
        });

        env.ledger().with_mut(|li| li.timestamp += 48 * 60 * 60);

        let err =
            env.as_contract(&contract_addr, || InvoiceRegistry::execute_upgrade(env.clone()));
        assert_eq!(err, Err(ContractError::UpgradesPaused));
    }
}
