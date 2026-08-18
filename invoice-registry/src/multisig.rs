//! # Multi-party invoice verification with signature aggregation
//!
//! Requires M-of-N Ed25519 signatures (SME + buyer + optional third party)
//! before an invoice is considered verified. Each candidate signer is a
//! known Ed25519 public key configured up front; each submitted signature is
//! checked with `env.crypto().ed25519_verify` against a caller-supplied
//! message and rejected outright (the host traps) if it doesn't verify.
//! Partial progress is tracked on-chain until quorum (M) is reached, and the
//! whole verification auto-expires if quorum isn't reached within the
//! configured deadline.
//!
//! Scope note: the M-of-N config is keyed by `invoice_id` rather than by an
//! explicit "pool" identifier - invoice-registry has no first-class pool
//! concept of its own (pools live in `pool-manager`), so "configurable
//! M-of-N per pool" is realized here as "whoever configures verification for
//! an invoice picks the M-of-N appropriate for that invoice's pool policy".

use soroban_sdk::{contracttype, symbol_short, Bytes, BytesN, Env, Symbol, Vec};

const DEFAULT_TIMEOUT_SECS: u64 = 7 * 86_400; // 7 days

// Tuple keys `(tag, invoice_id)` rather than two single-field newtype
// structs wrapping the same `Symbol` - single-field tuple-struct storage
// keys that differ only by type name are easy to accidentally collide on in
// Soroban's storage-key encoding, since the encoding is driven by the
// value shape, not the Rust type name. A distinguishing tag Symbol as part
// of the key tuple makes collision structurally impossible.
const CONFIG_TAG: Symbol = symbol_short!("ms_cfg");
const STATE_TAG: Symbol = symbol_short!("ms_state");

fn config_key(invoice_id: &Symbol) -> (Symbol, Symbol) {
    (CONFIG_TAG, invoice_id.clone())
}

fn state_key(invoice_id: &Symbol) -> (Symbol, Symbol) {
    (STATE_TAG, invoice_id.clone())
}

/// The M-of-N policy for one invoice: `required` signatures out of the
/// listed `signers` (known Ed25519 public keys).
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationConfig {
    pub required: u32,
    pub signers: Vec<BytesN<32>>,
}

/// Partial/complete verification progress for one invoice.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationState {
    /// Public keys that have submitted a valid signature so far.
    pub collected: Vec<BytesN<32>>,
    /// Ledger timestamp after which submissions are rejected and the
    /// verification is considered expired if quorum wasn't reached.
    pub deadline: u64,
    /// `true` once `collected.len() >= config.required`.
    pub verified: bool,
}

/// Configure the M-of-N signer set for `invoice_id` and open a fresh
/// verification window (deadline = now + `DEFAULT_TIMEOUT_SECS`).
///
/// Panics if `required` is zero or exceeds the number of signers.
pub fn configure(env: &Env, invoice_id: Symbol, required: u32, signers: Vec<BytesN<32>>) {
    assert!(required > 0, "required must be positive");
    assert!(
        (required as u32) <= signers.len(),
        "required cannot exceed the number of signers"
    );

    let config = VerificationConfig { required, signers };
    env.storage()
        .persistent()
        .set(&config_key(&invoice_id), &config);

    let state = VerificationState {
        collected: Vec::new(env),
        deadline: env.ledger().timestamp() + DEFAULT_TIMEOUT_SECS,
        verified: false,
    };
    env.storage().persistent().set(&state_key(&invoice_id), &state);
}

/// Submit one signature over `message` from `signer_pubkey` toward
/// `invoice_id`'s quorum. Returns `true` if this submission reached quorum.
///
/// Panics if: verification wasn't configured for this invoice, the
/// verification is already complete, the deadline has passed, the public
/// key isn't one of the configured signers, the same signer has already
/// submitted, or the signature doesn't verify against `message`.
pub fn submit_signature(
    env: &Env,
    invoice_id: Symbol,
    signer_pubkey: BytesN<32>,
    message: Bytes,
    signature: BytesN<64>,
) -> bool {
    let config: VerificationConfig = env
        .storage()
        .persistent()
        .get(&config_key(&invoice_id))
        .expect("verification not configured for this invoice");
    let mut state: VerificationState = env
        .storage()
        .persistent()
        .get(&state_key(&invoice_id))
        .expect("verification not configured for this invoice");

    assert!(!state.verified, "verification already reached quorum");
    assert!(
        env.ledger().timestamp() <= state.deadline,
        "verification window has expired"
    );
    assert!(
        config.signers.contains(&signer_pubkey),
        "unknown signer public key"
    );
    assert!(
        !state.collected.contains(&signer_pubkey),
        "signer has already submitted"
    );

    // Traps (aborts the whole invocation) if the signature doesn't verify -
    // an invalid or tampered signature/message never gets recorded.
    env.crypto()
        .ed25519_verify(&signer_pubkey, &message, &signature);

    state.collected.push_back(signer_pubkey);
    if state.collected.len() >= config.required {
        state.verified = true;
    }

    let reached = state.verified;
    env.storage().persistent().set(&state_key(&invoice_id), &state);
    reached
}

/// Current verification state for `invoice_id`.
///
/// Panics if verification wasn't configured for this invoice.
pub fn status(env: &Env, invoice_id: Symbol) -> VerificationState {
    env.storage()
        .persistent()
        .get(&state_key(&invoice_id))
        .expect("verification not configured for this invoice")
}

/// Whether `invoice_id`'s verification window has expired without reaching
/// quorum (auto-reject condition from the issue's acceptance criteria).
///
/// Panics if verification wasn't configured for this invoice.
pub fn is_expired(env: &Env, invoice_id: Symbol) -> bool {
    let state: VerificationState = env
        .storage()
        .persistent()
        .get(&state_key(&invoice_id))
        .expect("verification not configured for this invoice");
    !state.verified && env.ledger().timestamp() > state.deadline
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;
    use soroban_sdk::{testutils::Ledger, Address};

    // Returns the raw 32-byte public key rather than a `BytesN` - `BytesN`
    // values are tied to the specific `Env` host instance that created
    // them, and mixing values from two different `Env::default()`
    // instances trips Soroban's object-integrity checks. Callers convert
    // to `BytesN` themselves using the one real `Env` the test actually
    // uses.
    fn keypair() -> ([u8; 32], SigningKey) {
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        (signing_key.verifying_key().to_bytes(), signing_key)
    }

    fn sign(env: &Env, signing_key: &SigningKey, msg: &[u8]) -> (Bytes, BytesN<64>) {
        let sig = signing_key.sign(msg);
        let message = Bytes::from_slice(env, msg);
        let signature = BytesN::from_array(env, &sig.to_bytes());
        (message, signature)
    }

    struct Fixture {
        env: Env,
        addr: Address,
        invoice_id: Symbol,
        signers: [(BytesN<32>, SigningKey); 3],
    }

    fn setup(required: u32) -> Fixture {
        let env = Env::default();
        env.mock_all_auths();
        let addr = env.register_contract(None::<&Address>, crate::InvoiceRegistry);
        let invoice_id = Symbol::new(&env, "INV1");

        let raw_signers = [keypair(), keypair(), keypair()];
        let signers: [(BytesN<32>, SigningKey); 3] =
            raw_signers.map(|(pk, sk)| (BytesN::from_array(&env, &pk), sk));
        let signer_keys: Vec<BytesN<32>> = Vec::from_array(
            &env,
            [
                signers[0].0.clone(),
                signers[1].0.clone(),
                signers[2].0.clone(),
            ],
        );

        env.as_contract(&addr, || {
            configure(&env, invoice_id.clone(), required, signer_keys);
        });

        Fixture {
            env,
            addr,
            invoice_id,
            signers,
        }
    }

    #[test]
    fn valid_signature_from_configured_signer_is_accepted() {
        let f = setup(2);
        f.env.as_contract(&f.addr, || {
            let (msg, sig) = sign(&f.env, &f.signers[0].1, b"invoice INV1 amount 50000");
            let reached =
                submit_signature(&f.env, f.invoice_id.clone(), f.signers[0].0.clone(), msg, sig);
            assert!(!reached);
            let state = status(&f.env, f.invoice_id.clone());
            assert_eq!(state.collected.len(), 1);
            assert!(!state.verified);
        });
    }

    #[test]
    #[should_panic(expected = "unknown signer public key")]
    fn signature_from_unknown_key_is_rejected() {
        let f = setup(2);
        let (unknown_pubkey, unknown_key) = keypair();
        f.env.as_contract(&f.addr, || {
            let unknown_pubkey = BytesN::from_array(&f.env, &unknown_pubkey);
            let (msg, sig) = sign(&f.env, &unknown_key, b"invoice INV1 amount 50000");
            submit_signature(&f.env, f.invoice_id.clone(), unknown_pubkey, msg, sig);
        });
    }

    #[test]
    #[should_panic(expected = "signer has already submitted")]
    fn duplicate_signature_from_same_signer_does_not_double_count() {
        let f = setup(2);
        f.env.as_contract(&f.addr, || {
            let (msg1, sig1) = sign(&f.env, &f.signers[0].1, b"invoice INV1 amount 50000");
            submit_signature(&f.env, f.invoice_id.clone(), f.signers[0].0.clone(), msg1, sig1);

            let (msg2, sig2) = sign(&f.env, &f.signers[0].1, b"invoice INV1 amount 50000");
            submit_signature(&f.env, f.invoice_id.clone(), f.signers[0].0.clone(), msg2, sig2);
        });
    }

    #[test]
    fn quorum_reached_exactly_at_m_signatures_flips_state_to_verified() {
        let f = setup(2);
        f.env.as_contract(&f.addr, || {
            let (msg0, sig0) = sign(&f.env, &f.signers[0].1, b"invoice INV1 amount 50000");
            let reached0 =
                submit_signature(&f.env, f.invoice_id.clone(), f.signers[0].0.clone(), msg0, sig0);
            assert!(!reached0);

            let (msg1, sig1) = sign(&f.env, &f.signers[1].1, b"invoice INV1 amount 50000");
            let reached1 =
                submit_signature(&f.env, f.invoice_id.clone(), f.signers[1].0.clone(), msg1, sig1);
            assert!(reached1);

            let state = status(&f.env, f.invoice_id.clone());
            assert!(state.verified);
            assert_eq!(state.collected.len(), 2);
        });
    }

    #[test]
    fn quorum_not_reached_before_deadline_stays_pending() {
        let f = setup(2);
        f.env.as_contract(&f.addr, || {
            let (msg, sig) = sign(&f.env, &f.signers[0].1, b"invoice INV1 amount 50000");
            submit_signature(&f.env, f.invoice_id.clone(), f.signers[0].0.clone(), msg, sig);

            assert!(!is_expired(&f.env, f.invoice_id.clone()));
            let state = status(&f.env, f.invoice_id.clone());
            assert!(!state.verified);
        });
    }

    #[test]
    #[should_panic(expected = "verification window has expired")]
    fn submitting_after_deadline_is_rejected_even_with_a_valid_signature() {
        let f = setup(2);
        f.env.ledger().with_mut(|li| li.timestamp += DEFAULT_TIMEOUT_SECS + 1);
        f.env.as_contract(&f.addr, || {
            assert!(is_expired(&f.env, f.invoice_id.clone()));
            let (msg, sig) = sign(&f.env, &f.signers[0].1, b"invoice INV1 amount 50000");
            submit_signature(&f.env, f.invoice_id.clone(), f.signers[0].0.clone(), msg, sig);
        });
    }

    #[test]
    #[should_panic]
    fn tampered_message_is_rejected() {
        let f = setup(1);
        f.env.as_contract(&f.addr, || {
            let (_original_msg, sig) = sign(&f.env, &f.signers[0].1, b"invoice INV1 amount 50000");
            // Sign a different message than the one we submit - the host
            // traps inside ed25519_verify since the signature won't verify
            // against this message.
            let tampered_msg = Bytes::from_slice(&f.env, b"invoice INV1 amount 99999");
            submit_signature(
                &f.env,
                f.invoice_id.clone(),
                f.signers[0].0.clone(),
                tampered_msg,
                sig,
            );
        });
    }

    #[test]
    #[should_panic]
    fn tampered_signature_is_rejected() {
        let f = setup(1);
        f.env.as_contract(&f.addr, || {
            let (msg, sig_bytes) = sign(&f.env, &f.signers[0].1, b"invoice INV1 amount 50000");
            // Flip a byte in the signature - the host traps inside
            // ed25519_verify since the tampered signature won't verify.
            let mut raw = sig_bytes.to_array();
            raw[0] ^= 0xFF;
            let tampered_sig = BytesN::from_array(&f.env, &raw);
            submit_signature(&f.env, f.invoice_id.clone(), f.signers[0].0.clone(), msg, tampered_sig);
        });
    }

    #[test]
    #[should_panic(expected = "required cannot exceed the number of signers")]
    fn configure_rejects_required_greater_than_signer_count() {
        let env = Env::default();
        env.mock_all_auths();
        let addr = env.register_contract(None::<&Address>, crate::InvoiceRegistry);
        let (pk, _) = keypair();
        let pk = BytesN::from_array(&env, &pk);
        env.as_contract(&addr, || {
            configure(&env, Symbol::new(&env, "INVX"), 5, Vec::from_array(&env, [pk]));
        });
    }
}
