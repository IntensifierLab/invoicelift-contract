#![no_std]

//! # Invoice Registry — Confidential Invoice Amounts
//!
//! Invoice amounts are stored as Pedersen commitments — no plaintext values
//! are ever written on-chain. Only parties holding the opening `(value, blinding)`
//! can verify (or prove to an auditor) the original amount.

mod pedersen;

use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Env, Symbol};

// ─── Storage Keys ──────────────────────────────────────────────────────────

mod storage {
    use soroban_sdk::{symbol_short, Symbol};

    pub const ADMIN: Symbol = symbol_short!("admin");
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
}

// ─── Contract ──────────────────────────────────────────────────────────────



/// Invoice lifecycle and verification.
#[contract]
pub struct InvoiceRegistry;

#[contractimpl]
impl InvoiceRegistry {
    /// One-time initialization. Sets the initial administrator.
    ///
    /// Returns [`Error::AlreadyInitialized`] if the contract has already been
    /// initialized, so the admin can never be silently overwritten.
    pub fn initialize(env: Env, admin: Symbol) {
        if env.storage().instance().has(&storage::ADMIN) {
            panic!("already initialized");
        }
        env.storage()
            .instance()
            .set(&storage::ADMIN, &admin);
    }

    // ── Invoice lifecycle ────────────────────────────────────────────────

    /// Register a new invoice with a Pedersen commitment of the amount.
    ///
    /// The SME computes `commitment = pedersen::commit(amount, blinding)` off-chain
    /// and submits only the commitment. The plaintext amount and blinding factor
    /// remain secret.
    pub fn register(env: Env, id: Symbol, commitment: i128, owner: Symbol) {
        let key = InvoiceKey(id.clone());
        if env.storage().persistent().has(&key) {
            panic!("invoice already exists");
        }

        let invoice = CommittedInvoice {
            id,
            commitment,
            status: InvoiceStatus::Pending,
            owner,
        };

        env.storage().persistent().set(&key, &invoice);
    }

    /// Admin approves a pending invoice (Pending → Approved).
    pub fn approve(env: Env, caller: Symbol, id: Symbol) {
        Self::require_admin(&env, &caller);

        let key = InvoiceKey(id);
        let mut invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&key)
            .expect("invoice not found");

        assert!(
            invoice.status == InvoiceStatus::Pending,
            "invoice must be Pending to approve"
        );

        invoice.status = InvoiceStatus::Approved;
        env.storage().persistent().set(&key, &invoice);
    }

    /// Admin assigns an approved invoice to a new owner / pool (Approved → Assigned).
    pub fn assign(env: Env, caller: Symbol, id: Symbol, new_owner: Symbol) {
        Self::require_admin(&env, &caller);

        let key = InvoiceKey(id);
        let mut invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&key)
            .expect("invoice not found");

        assert!(
            invoice.status == InvoiceStatus::Approved,
            "invoice must be Approved to assign"
        );

        invoice.status = InvoiceStatus::Assigned;
        invoice.owner = new_owner;
        env.storage().persistent().set(&key, &invoice);
    }

    /// Admin marks an assigned invoice as repaid (Assigned → Repaid).
    pub fn mark_repaid(env: Env, caller: Symbol, id: Symbol) {
        Self::require_admin(&env, &caller);

        let key = InvoiceKey(id);
        let mut invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&key)
            .expect("invoice not found");

        assert!(
            invoice.status == InvoiceStatus::Assigned,
            "invoice must be Assigned to mark repaid"
        );

        invoice.status = InvoiceStatus::Repaid;
        env.storage().persistent().set(&key, &invoice);
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
    pub fn verify_amount(env: Env, id: Symbol, value: i128, blinding: i128) -> bool {
        let invoice: CommittedInvoice = env
            .storage()
            .persistent()
            .get(&InvoiceKey(id))
            .expect("invoice not found");

        pedersen::verify(invoice.commitment, value, blinding)
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

    fn require_admin(env: &Env, caller: &Symbol) {
        let admin: Symbol = env
            .storage()
            .instance()
            .get(&storage::ADMIN)
            .expect("not initialized");
        assert!(*caller == admin, "only admin");
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
            InvoiceRegistry::initialize(env.clone(), symbol_short!("admin"));
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
            );
        });

        let invoice = env.as_contract(&contract_addr, || {
            InvoiceRegistry::get_invoice(env.clone(), inv_id).expect("invoice missing")
        });

        assert_eq!(invoice.commitment, commitment);
        assert_eq!(invoice.status, InvoiceStatus::Pending);
        assert_eq!(invoice.owner, symbol_short!("sme1"));
    }

    #[test]
    #[should_panic(expected = "invoice already exists")]
    fn test_register_duplicate_panics() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV002");
        let commitment = test_commitment(10_000, 999);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            );
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            );
        });
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
            );
            InvoiceRegistry::approve(env.clone(), symbol_short!("admin"), inv_id.clone());
        });

        let invoice = env.as_contract(&contract_addr, || {
            InvoiceRegistry::get_invoice(env.clone(), inv_id).expect("missing")
        });
        assert_eq!(invoice.status, InvoiceStatus::Approved);
    }

    #[test]
    #[should_panic(expected = "only admin")]
    fn test_approve_non_admin_panics() {
        let (env, contract_addr) = setup();
        let inv_id = symbol_short!("INV004");
        let commitment = test_commitment(30_000, 777);

        env.as_contract(&contract_addr, || {
            InvoiceRegistry::register(
                env.clone(),
                inv_id.clone(),
                commitment,
                symbol_short!("sme1"),
            );
            InvoiceRegistry::approve(env.clone(), symbol_short!("hacker"), inv_id);
        });
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
            );
            InvoiceRegistry::approve(env.clone(), symbol_short!("admin"), inv_id.clone());
            InvoiceRegistry::assign(
                env.clone(),
                symbol_short!("admin"),
                inv_id.clone(),
                symbol_short!("pool1"),
            );
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
            );
            InvoiceRegistry::approve(env.clone(), symbol_short!("admin"), inv_id.clone());
            InvoiceRegistry::assign(
                env.clone(),
                symbol_short!("admin"),
                inv_id.clone(),
                symbol_short!("pool1"),
            );
            InvoiceRegistry::mark_repaid(env.clone(), symbol_short!("admin"), inv_id.clone());
        });

        let invoice = env.as_contract(&contract_addr, || {
            InvoiceRegistry::get_invoice(env.clone(), inv_id).expect("missing")
        });
        assert_eq!(invoice.status, InvoiceStatus::Repaid);
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
            );
        });

        let verified = env.as_contract(&contract_addr, || {
            InvoiceRegistry::verify_amount(env.clone(), inv_id, value, blinding)
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
            );
        });

        let verified = env.as_contract(&contract_addr, || {
            InvoiceRegistry::verify_amount(env.clone(), inv_id, 99_999, 13579)
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
            );
            assert_eq!(
                InvoiceRegistry::get_invoice(env.clone(), inv_id.clone())
                    .unwrap()
                    .status,
                InvoiceStatus::Pending
            );

            // Approve
            InvoiceRegistry::approve(env.clone(), symbol_short!("admin"), inv_id.clone());
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
            );
            assert_eq!(
                InvoiceRegistry::get_invoice(env.clone(), inv_id.clone())
                    .unwrap()
                    .status,
                InvoiceStatus::Assigned
            );

            // Mark repaid
            InvoiceRegistry::mark_repaid(env.clone(), symbol_short!("admin"), inv_id.clone());
            assert_eq!(
                InvoiceRegistry::get_invoice(env.clone(), inv_id.clone())
                    .unwrap()
                    .status,
                InvoiceStatus::Repaid
            );

            // Verify amount at any stage
            assert!(InvoiceRegistry::verify_amount(
                env.clone(),
                inv_id,
                value,
                blinding
            ));
        });
    }
}
