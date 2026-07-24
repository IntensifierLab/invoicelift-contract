extern crate std;

use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Events},
    Address, Env, IntoVal,
};

use crate::{Error, InvoiceRegistry, InvoiceRegistryClient};

fn setup<'a>(env: &'a Env) -> (InvoiceRegistryClient<'a>, Address) {
    let contract_id = env.register_contract(None, InvoiceRegistry);
    let client = InvoiceRegistryClient::new(env, &contract_id);
    let admin = Address::generate(env);
    client.initialize(&admin);
    (client, admin)
}

#[test]
fn initialize_sets_admin() {
    let env = Env::default();
    let (client, admin) = setup(&env);
    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_pending_admin(), None);
}

#[test]
fn initialize_twice_errors() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    let other = Address::generate(&env);
    assert_eq!(
        client.try_initialize(&other),
        Err(Ok(Error::AlreadyInitialized))
    );
}

#[test]
fn get_admin_before_initialize_errors() {
    let env = Env::default();
    let contract_id = env.register_contract(None, InvoiceRegistry);
    let client = InvoiceRegistryClient::new(&env, &contract_id);
    assert_eq!(client.try_get_admin(), Err(Ok(Error::NotInitialized)));
}

#[test]
fn transfer_admin_records_pending_without_changing_admin() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, admin) = setup(&env);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);

    // Admin is unchanged until the nominee accepts.
    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_pending_admin(), Some(new_admin));
}

#[test]
fn transfer_admin_requires_current_admin_auth() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, admin) = setup(&env);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);

    // The authorization recorded for transfer_admin belongs to the current admin.
    let auths = env.auths();
    assert_eq!(auths.first().map(|(addr, _)| addr.clone()), Some(admin));
}

#[test]
fn accept_admin_promotes_pending_and_clears_it() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.accept_admin();

    assert_eq!(client.get_admin(), new_admin);
    assert_eq!(client.get_pending_admin(), None);
}

#[test]
fn accept_admin_emits_admin_transferred_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, admin) = setup(&env);
    let new_admin = Address::generate(&env);

    client.transfer_admin(&new_admin);
    client.accept_admin();

    let events = env.events().all();
    let (_, topics, data) = events.last().unwrap();
    assert_eq!(
        topics,
        (symbol_short!("adm_xfer"), admin, new_admin.clone()).into_val(&env)
    );
    let data_admin: Address = data.into_val(&env);
    assert_eq!(data_admin, new_admin);
}

#[test]
fn accept_admin_without_pending_errors() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin) = setup(&env);
    assert_eq!(client.try_accept_admin(), Err(Ok(Error::NoPendingAdmin)));
}

#[test]
fn transfer_can_be_overwritten_before_acceptance() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, admin) = setup(&env);
    let first = Address::generate(&env);
    let second = Address::generate(&env);

    client.transfer_admin(&first);
    client.transfer_admin(&second);
    assert_eq!(client.get_pending_admin(), Some(second.clone()));

    client.accept_admin();
    assert_eq!(client.get_admin(), second);
    // The superseded nominee never gained control.
    assert_ne!(client.get_admin(), first);
    assert_ne!(client.get_admin(), admin);
}

#[test]
fn ping_and_version_are_unaffected() {
    let env = Env::default();
    let (client, _admin) = setup(&env);
    assert_eq!(client.ping(&symbol_short!("hello")), symbol_short!("hello"));
    assert_eq!(client.version(), 1);
}
