//! # Invoice NFT tokenisation
//!
//! Tokenises a financed invoice as a transferable on-chain asset: one token
//! per invoice, identified by the invoice's own `Symbol` id (there's no
//! separate numeric token-id scheme - the invoice already has a stable
//! unique identifier, so reusing it keeps the model coherent with the rest
//! of this file instead of inventing a second identity for the same thing).
//!
//! ## Design choices (documented, not shortcuts)
//!
//! - **Ownership identity**: `Symbol`, matching every other identity in
//!   this file (`CommittedInvoice.owner`, the `admin`/`caller` pattern).
//!   None of `invoice-registry`'s existing entrypoints use `Address` +
//!   `require_auth()` - they all check a caller-supplied `Symbol` against a
//!   stored one. Introducing real `Address`-based authorization for just
//!   this one feature would leave the contract with two incompatible
//!   ownership/auth models side by side. A stronger `Address`-based
//!   ownership model is a natural follow-up for the whole contract, not a
//!   gap specific to NFTs.
//! - **Royalty hook**: `transfer` takes a `royalty_bps` parameter and emits
//!   it (with the previous owner) as an event on every transfer, rather
//!   than moving a payment itself. There's no payment token or amount in
//!   scope for this call (invoice-registry doesn't hold funds - pool-manager
//!   does), so the royalty is a hook other layers (an off-chain settlement
//!   job, or a future on-chain marketplace contract) can subscribe to and
//!   act on, not a value transfer this contract performs itself.
//! - **"Owner earns repayment proportional to ownership"**: this model has
//!   exactly one owner per token (no fractional/multi-owner cap table), so
//!   "proportional to ownership" simplifies to "the current owner receives
//!   the full repayment". `repayment_share` returns `(current_owner,
//!   total_repayment)` on that basis. A fractional cap-table (many owners,
//!   each with a basis-points share) is a real, meaningfully bigger feature
//!   and is left as a documented follow-up rather than bolted on here.

use soroban_sdk::{contracttype, symbol_short, Env, Symbol, Vec};

const OWNER_TAG: Symbol = symbol_short!("nft_own");
const HIST_TAG: Symbol = symbol_short!("nft_hist");

fn owner_key(token_id: &Symbol) -> (Symbol, Symbol) {
    (OWNER_TAG, token_id.clone())
}

fn history_key(token_id: &Symbol) -> (Symbol, Symbol) {
    (HIST_TAG, token_id.clone())
}

/// One recorded transfer of ownership.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferRecord {
    pub from: Symbol,
    pub to: Symbol,
    pub timestamp: u64,
    pub royalty_bps: u32,
}

/// Mint the token for `token_id` (typically called when an invoice becomes
/// `Assigned`/financed) with `owner` as its initial owner.
///
/// Panics if this token has already been minted.
pub fn mint(env: &Env, token_id: Symbol, owner: Symbol) {
    let key = owner_key(&token_id);
    assert!(!env.storage().persistent().has(&key), "token already minted");
    env.storage().persistent().set(&key, &owner);
    env.storage()
        .persistent()
        .set(&history_key(&token_id), &Vec::<TransferRecord>::new(env));
}

/// Transfer `token_id` from `from` to `to`, recording the transfer in its
/// on-chain history and publishing a royalty-hook event
/// (`("nft_xfer",), (token_id, from, royalty_bps)`) for other layers to act
/// on.
///
/// Panics if the token isn't minted, if `from` isn't the current owner, or
/// if `royalty_bps` exceeds 10,000 (100%).
pub fn transfer(env: &Env, token_id: Symbol, from: Symbol, to: Symbol, royalty_bps: u32) {
    assert!(royalty_bps <= 10_000, "royalty_bps must be at most 10000");

    let key = owner_key(&token_id);
    let current: Symbol = env
        .storage()
        .persistent()
        .get(&key)
        .expect("token not minted");
    assert!(current == from, "caller is not the current owner");

    env.storage().persistent().set(&key, &to);

    let hkey = history_key(&token_id);
    let mut history: Vec<TransferRecord> = env
        .storage()
        .persistent()
        .get(&hkey)
        .unwrap_or_else(|| Vec::new(env));
    history.push_back(TransferRecord {
        from: from.clone(),
        to: to.clone(),
        timestamp: env.ledger().timestamp(),
        royalty_bps,
    });
    env.storage().persistent().set(&hkey, &history);

    env.events()
        .publish((symbol_short!("nft_xfer"),), (token_id, from, royalty_bps));
}

/// Current owner of `token_id`, or `None` if it hasn't been minted.
pub fn owner_of(env: &Env, token_id: Symbol) -> Option<Symbol> {
    env.storage().persistent().get(&owner_key(&token_id))
}

/// Full on-chain transfer history for `token_id`, oldest first. Empty if
/// the token hasn't been minted or has never been transferred.
pub fn transfer_history(env: &Env, token_id: Symbol) -> Vec<TransferRecord> {
    env.storage()
        .persistent()
        .get(&history_key(&token_id))
        .unwrap_or_else(|| Vec::new(env))
}

/// The current owner's share of `total_repayment`. Single-owner model (see
/// module docs): the current owner receives the full amount.
///
/// Panics if the token hasn't been minted.
pub fn repayment_share(env: &Env, token_id: Symbol, total_repayment: i128) -> (Symbol, i128) {
    let owner: Symbol = env
        .storage()
        .persistent()
        .get(&owner_key(&token_id))
        .expect("token not minted");
    (owner, total_repayment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{symbol_short as sym, Address};

    fn setup() -> (Env, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let addr = env.register_contract(None::<&Address>, crate::InvoiceRegistry);
        (env, addr)
    }

    #[test]
    fn mint_succeeds_and_sets_initial_owner() {
        let (env, addr) = setup();
        env.as_contract(&addr, || {
            mint(&env, sym!("INV1"), sym!("sme1"));
            assert_eq!(owner_of(&env, sym!("INV1")), Some(sym!("sme1")));
        });
    }

    #[test]
    #[should_panic(expected = "token already minted")]
    fn mint_twice_panics() {
        let (env, addr) = setup();
        env.as_contract(&addr, || {
            mint(&env, sym!("INV1"), sym!("sme1"));
            mint(&env, sym!("INV1"), sym!("sme1"));
        });
    }

    #[test]
    #[should_panic(expected = "caller is not the current owner")]
    fn transfer_from_non_owner_panics() {
        let (env, addr) = setup();
        env.as_contract(&addr, || {
            mint(&env, sym!("INV1"), sym!("sme1"));
            transfer(&env, sym!("INV1"), sym!("hacker"), sym!("pool1"), 500);
        });
    }

    #[test]
    fn transfer_updates_owner_and_appends_exactly_one_history_record() {
        let (env, addr) = setup();
        env.as_contract(&addr, || {
            mint(&env, sym!("INV1"), sym!("sme1"));
            transfer(&env, sym!("INV1"), sym!("sme1"), sym!("pool1"), 250);

            assert_eq!(owner_of(&env, sym!("INV1")), Some(sym!("pool1")));
            let history = transfer_history(&env, sym!("INV1"));
            assert_eq!(history.len(), 1);
            let record = history.get(0).unwrap();
            assert_eq!(record.from, sym!("sme1"));
            assert_eq!(record.to, sym!("pool1"));
            assert_eq!(record.royalty_bps, 250);
        });
    }

    #[test]
    fn transfer_history_accumulates_across_multiple_transfers() {
        let (env, addr) = setup();
        env.as_contract(&addr, || {
            mint(&env, sym!("INV1"), sym!("sme1"));
            transfer(&env, sym!("INV1"), sym!("sme1"), sym!("pool1"), 100);
            transfer(&env, sym!("INV1"), sym!("pool1"), sym!("buyer1"), 200);

            let history = transfer_history(&env, sym!("INV1"));
            assert_eq!(history.len(), 2);
            assert_eq!(history.get(0).unwrap().to, sym!("pool1"));
            assert_eq!(history.get(1).unwrap().to, sym!("buyer1"));
            assert_eq!(owner_of(&env, sym!("INV1")), Some(sym!("buyer1")));
        });
    }

    #[test]
    fn repayment_share_returns_current_owner_with_full_amount() {
        let (env, addr) = setup();
        env.as_contract(&addr, || {
            mint(&env, sym!("INV1"), sym!("sme1"));
            transfer(&env, sym!("INV1"), sym!("sme1"), sym!("pool1"), 0);

            let (owner, amount) = repayment_share(&env, sym!("INV1"), 40_000);
            assert_eq!(owner, sym!("pool1"));
            assert_eq!(amount, 40_000);
        });
    }

    #[test]
    fn querying_an_unminted_token_returns_none_or_empty() {
        let (env, addr) = setup();
        env.as_contract(&addr, || {
            assert_eq!(owner_of(&env, sym!("GHOST")), None);
            assert_eq!(transfer_history(&env, sym!("GHOST")).len(), 0);
        });
    }

    #[test]
    #[should_panic(expected = "royalty_bps must be at most 10000")]
    fn transfer_rejects_royalty_over_10000_bps() {
        let (env, addr) = setup();
        env.as_contract(&addr, || {
            mint(&env, sym!("INV1"), sym!("sme1"));
            transfer(&env, sym!("INV1"), sym!("sme1"), sym!("pool1"), 10_001);
        });
    }
}
