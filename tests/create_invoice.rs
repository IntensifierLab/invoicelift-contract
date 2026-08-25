//! Acceptance tests for `create_invoice` (issue #13).
//!
//! Kept in their own integration test file rather than appended to the
//! in-crate `mod tests`, so this change touches no region of `lib.rs` that
//! other in-flight work also edits.

use invoice_registry::{ContractError, InvoiceRegistry, InvoiceRegistryClient, InvoiceStatus};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events, Ledger},
    vec, Address, Env, IntoVal, Symbol,
};

const DUE: u64 = 10_000;

struct Fixture<'a> {
    env: Env,
    client: InvoiceRegistryClient<'a>,
    contract_id: Address,
    sme: Address,
}

/// Deploy an initialized registry with the ledger clock at a fixed point, so
/// "due date in the future" means something deterministic.
fn setup() -> Fixture<'static> {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|li| li.timestamp = 1_000);

    let id = env.register_contract(None::<&Address>, InvoiceRegistry);
    let client = InvoiceRegistryClient::new(&env, &id);
    client.initialize(&symbol_short!("admin"));

    let sme = Address::generate(&env);
    Fixture {
        env,
        client,
        contract_id: id,
        sme,
    }
}

impl Fixture<'_> {
    fn create(&self, id: Symbol, buyer: Symbol, commitment: i128, due: u64) {
        self.client.create_invoice(
            &self.sme,
            &symbol_short!("sme1"),
            &id,
            &buyer,
            &commitment,
            &due,
        );
    }

    /// Attempt a creation expected to fail, returning the contract error.
    fn create_err(&self, id: Symbol, buyer: Symbol, commitment: i128, due: u64) -> ContractError {
        self.client
            .try_create_invoice(
                &self.sme,
                &symbol_short!("sme1"),
                &id,
                &buyer,
                &commitment,
                &due,
            )
            .expect_err("expected create_invoice to be rejected")
            .expect("expected a contract error, not a host error")
    }

    /// Attempt a creation that may fail, discarding the outcome.
    fn try_create(&self, id: Symbol, buyer: Symbol, commitment: i128, due: u64) {
        let _ = self.client.try_create_invoice(
            &self.sme,
            &symbol_short!("sme1"),
            &id,
            &buyer,
            &commitment,
            &due,
        );
    }
}

// ── Happy path: stored by id, in both records ────────────────────────────────

#[test]
fn stores_the_invoice_and_its_terms_under_the_given_id() {
    let f = setup();
    f.create(symbol_short!("inv1"), symbol_short!("buyer1"), 42, DUE);

    let invoice = f
        .client
        .get_invoice(&symbol_short!("inv1"))
        .expect("lifecycle record should exist");
    assert_eq!(invoice.id, symbol_short!("inv1"));
    assert_eq!(invoice.commitment, 42);
    assert_eq!(invoice.status, InvoiceStatus::Pending);
    assert_eq!(invoice.owner, symbol_short!("sme1"));
    assert!(!invoice.frozen);
    assert!(!invoice.fraud_flagged);

    let terms = f
        .client
        .invoice_terms(&symbol_short!("inv1"))
        .expect("terms record should exist");
    assert_eq!(terms.buyer, symbol_short!("buyer1"));
    assert_eq!(terms.commitment, 42);
    assert_eq!(terms.due_date, DUE);
    assert_eq!(terms.sme, f.sme);
    assert_eq!(terms.created_at, 1_000, "should stamp the ledger timestamp");
}

#[test]
fn distinct_ids_do_not_collide() {
    let f = setup();
    f.create(symbol_short!("inv1"), symbol_short!("buyer1"), 42, DUE);
    f.create(symbol_short!("inv2"), symbol_short!("buyer2"), 99, DUE + 5);

    assert_eq!(
        f.client
            .get_invoice(&symbol_short!("inv1"))
            .unwrap()
            .commitment,
        42
    );
    assert_eq!(
        f.client
            .get_invoice(&symbol_short!("inv2"))
            .unwrap()
            .commitment,
        99
    );
    assert_eq!(
        f.client
            .invoice_terms(&symbol_short!("inv2"))
            .unwrap()
            .buyer,
        symbol_short!("buyer2")
    );
}

#[test]
fn terms_key_does_not_overwrite_the_lifecycle_record() {
    // Both records are keyed on the same invoice id. `#[contracttype]` encodes
    // a tuple struct as a bare vec of its fields with no type-name
    // discriminant, so without the marker in `TermsKey` these two would share
    // one ledger slot and the second write would clobber the first.
    let f = setup();
    f.create(symbol_short!("inv1"), symbol_short!("buyer1"), 42, DUE);

    assert!(f.client.get_invoice(&symbol_short!("inv1")).is_some());
    assert!(f.client.invoice_terms(&symbol_short!("inv1")).is_some());
}

// ── Schema validation ────────────────────────────────────────────────────────

#[test]
fn rejects_a_zero_commitment() {
    let f = setup();
    assert_eq!(
        f.create_err(symbol_short!("inv1"), symbol_short!("buyer1"), 0, DUE),
        ContractError::InvalidCommitment
    );
}

#[test]
fn rejects_a_due_date_in_the_past() {
    let f = setup();
    assert_eq!(
        f.create_err(symbol_short!("inv1"), symbol_short!("buyer1"), 42, 999),
        ContractError::InvalidDueDate
    );
}

#[test]
fn rejects_a_due_date_equal_to_now() {
    // An invoice due at the current ledger timestamp is already defaulted the
    // moment it exists, which the repayment waterfall has no handling for.
    let f = setup();
    assert_eq!(
        f.create_err(symbol_short!("inv1"), symbol_short!("buyer1"), 42, 1_000),
        ContractError::InvalidDueDate
    );
}

#[test]
fn rejects_an_invoice_the_sme_raised_against_itself() {
    let f = setup();
    assert_eq!(
        f.create_err(symbol_short!("inv1"), symbol_short!("sme1"), 42, DUE),
        ContractError::InvalidBuyer
    );
}

#[test]
fn a_rejected_invoice_writes_nothing() {
    // A validation failure that had already written one of the two records
    // would leave a half-created invoice behind.
    let f = setup();
    f.try_create(symbol_short!("inv1"), symbol_short!("buyer1"), 0, DUE);

    assert!(f.client.get_invoice(&symbol_short!("inv1")).is_none());
    assert!(f.client.invoice_terms(&symbol_short!("inv1")).is_none());
}

// ── Duplicate rejection ──────────────────────────────────────────────────────

#[test]
fn rejects_a_duplicate_id() {
    let f = setup();
    f.create(symbol_short!("inv1"), symbol_short!("buyer1"), 42, DUE);

    assert_eq!(
        f.create_err(symbol_short!("inv1"), symbol_short!("buyer2"), 99, DUE),
        ContractError::InvoiceAlreadyExists
    );
}

#[test]
fn a_duplicate_does_not_overwrite_the_original() {
    let f = setup();
    f.create(symbol_short!("inv1"), symbol_short!("buyer1"), 42, DUE);
    f.try_create(symbol_short!("inv1"), symbol_short!("buyer2"), 99, DUE);

    let terms = f.client.invoice_terms(&symbol_short!("inv1")).unwrap();
    assert_eq!(terms.buyer, symbol_short!("buyer1"));
    assert_eq!(terms.commitment, 42);
}

#[test]
fn rejects_an_id_already_taken_by_the_legacy_register_path() {
    // `register` writes only the lifecycle record. Checking just the terms key
    // would let an id registered the old way be silently re-created here.
    let f = setup();
    f.client
        .register(&symbol_short!("inv1"), &7, &symbol_short!("someone"));

    assert_eq!(
        f.create_err(symbol_short!("inv1"), symbol_short!("buyer1"), 42, DUE),
        ContractError::InvoiceAlreadyExists
    );
    assert_eq!(
        f.client.get_invoice(&symbol_short!("inv1")).unwrap().owner,
        symbol_short!("someone"),
        "the legacy record must be left intact"
    );
}

// ── Authorization ────────────────────────────────────────────────────────────

#[test]
fn requires_the_sme_address_to_sign() {
    let env = Env::default();
    env.ledger().with_mut(|li| li.timestamp = 1_000);
    let id = env.register_contract(None::<&Address>, InvoiceRegistry);
    let client = InvoiceRegistryClient::new(&env, &id);

    env.mock_all_auths();
    client.initialize(&symbol_short!("admin"));

    let sme = Address::generate(&env);
    client.create_invoice(
        &sme,
        &symbol_short!("sme1"),
        &symbol_short!("inv1"),
        &symbol_short!("buyer1"),
        &42,
        &DUE,
    );

    // The recorded authorization must name the SME address, not the admin or
    // the contract — this is the check that stops anyone raising an invoice in
    // someone else's name.
    let auths = env.auths();
    assert!(
        auths.iter().any(|(addr, _)| addr == &sme),
        "create_invoice must require_auth on the SME address, got {auths:?}"
    );
}

// Note: there is deliberately no "unsigned call panics" test here. A failed
// `require_auth` in the Soroban test host aborts the process rather than
// unwinding, so `#[should_panic]` cannot observe it and the whole test binary
// dies. `requires_the_sme_address_to_sign` above asserts the same property the
// reliable way, by inspecting the authorizations the call actually recorded.

// ── Event emission ───────────────────────────────────────────────────────────

#[test]
fn emits_an_invoice_created_event_carrying_the_terms() {
    let f = setup();
    f.create(symbol_short!("inv1"), symbol_short!("buyer1"), 42, DUE);

    // Compared as a whole vec: `Val` has no `PartialEq`, but `soroban_sdk::Vec`
    // compares with host-side deep equality, so this checks the topic and the
    // payload together.
    assert_eq!(
        f.env.events().all(),
        vec![
            &f.env,
            (
                f.contract_id.clone(),
                (symbol_short!("inv_new"), symbol_short!("inv1")).into_val(&f.env),
                (f.sme.clone(), symbol_short!("buyer1"), 42i128, DUE).into_val(&f.env),
            ),
        ],
        "the creation event should carry the invoice id as a topic and every \
         field an indexer needs as its payload"
    );
}

#[test]
fn a_rejected_invoice_emits_no_event() {
    // An event for a write that did not happen would put a phantom invoice
    // into every downstream index.
    let f = setup();
    f.try_create(symbol_short!("inv1"), symbol_short!("buyer1"), 0, DUE);

    assert_eq!(f.env.events().all().len(), 0);
}
