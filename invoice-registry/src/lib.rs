#![no_std]

//! # Invoice Registry — Confidential Invoice Amounts
//!
//! Invoice amounts are stored as Pedersen commitments — no plaintext values
//! are ever written on-chain. Only parties holding the opening `(value, blinding)`
//! can verify (or prove to an auditor) the original amount.

mod pedersen;

use soroban_sdk::{contract, contracterror, contractimpl, contracttype, symbol_short, Env, Symbol};

// ─── Storage Keys ──────────────────────────────────────────────────────────

mod storage {
    use soroban_sdk::{symbol_short, Symbol};

    pub const ADMIN: Symbol = symbol_short!("admin");
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
}

/// Per-invoice storage key.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvoiceKey(pub Symbol);

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
}
