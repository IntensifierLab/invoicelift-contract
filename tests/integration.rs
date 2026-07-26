use invoice_registry::{InvoiceRegistry, InvoiceRegistryClient};
use pool_manager::{PoolManager, PoolManagerClient};
use repayment_waterfall::{RepaymentWaterfall, RepaymentWaterfallClient};
use soroban_sdk::{symbol_short, testutils::Address as _, Address, Env};

/// Deploys `invoice-registry`, `pool-manager`, and `repayment-waterfall` into
/// one `Env` and initializes them, wiring `repayment-waterfall` to the
/// deployed `pool-manager` instance the way a real deployment would.
fn deploy_all(env: &Env) -> (InvoiceRegistryClient<'_>, PoolManagerClient<'_>, RepaymentWaterfallClient<'_>) {
    let registry_id = env.register_contract(None::<&Address>, InvoiceRegistry);
    let pool_id = env.register_contract(None::<&Address>, PoolManager);
    let waterfall_id = env.register_contract(None::<&Address>, RepaymentWaterfall);

    let registry = InvoiceRegistryClient::new(env, &registry_id);
    let pool = PoolManagerClient::new(env, &pool_id);
    let waterfall = RepaymentWaterfallClient::new(env, &waterfall_id);

    registry.initialize(&Address::generate(env));
    pool.initialize(&symbol_short!("admin"), &8_000);
    waterfall.initialize(&symbol_short!("admin"), &pool_id);

    (registry, pool, waterfall)
}

#[test]
fn deploys_all_three_contracts_in_the_same_env() {
    let env = Env::default();
    env.mock_all_auths();
    let (registry, pool, waterfall) = deploy_all(&env);

    assert_eq!(registry.version(), 1);
    assert_eq!(pool.version(), 1);
    assert_eq!(waterfall.version(), 1);
}

/// Happy-path cross-contract flow: an SME's invoice is acknowledged by the
/// registry, a lender funds the pool, the invoice is financed out of it, and
/// the buyer's eventual repayment is routed through the waterfall — which
/// itself cross-contract calls into pool-manager to credit the repaid
/// capital back to LPs.
#[test]
fn happy_path_invoice_financed_and_repaid_through_waterfall() {
    let env = Env::default();
    env.mock_all_auths();
    let (registry, pool, waterfall) = deploy_all(&env);

    let invoice_ref = symbol_short!("inv001");
    assert_eq!(registry.ping(&invoice_ref), invoice_ref);

    pool.deposit(&symbol_short!("lender1"), &100_000);
    pool.finance(&40_000);
    assert_eq!(pool.total_capital(), 100_000);
    assert_eq!(pool.financed_amount(), 40_000);
    assert_eq!(pool.reserve(), 60_000);

    waterfall.process_repayment(&40_000);
    assert_eq!(pool.total_capital(), 140_000);
    assert_eq!(pool.reserve(), 100_000);
}
