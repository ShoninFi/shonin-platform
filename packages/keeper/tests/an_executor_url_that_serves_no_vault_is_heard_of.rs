//! The keeper's `--executor-url` points somewhere that answers 404 to every
//! `/usage/...` (a wrong host or path, the API instead of the executor). A
//! 404 is taken at its word, as "serves no total for this vault": a 404 for
//! one vault can be right (another executor serves it). But a 404 for every
//! vault, with none ever served a total, for an hour (here a few seconds) is
//! the misconfiguration the per-vault warn line names, and no bill would be
//! collected: the keeper's health says so, and so does a warn line.

mod process;

use std::time::Duration;

use alloy::primitives::{Address, U256};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    FACTORY, IReplacesFactory, STATION, a_spec, answers, node, serve, serve_node,
    start_keeper_serving_none_for, timers,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_executor_url_that_serves_no_vault_is_heard_of() {
    let start = 1_700_000_000u64;
    let node = node(start);
    let vaults = [Address::repeat_byte(0x71), Address::repeat_byte(0x72)];
    answers(
        &node,
        FACTORY,
        IVaultFactory::vaultsCall {},
        vaults.to_vec(),
    );
    answers(
        &node,
        FACTORY,
        IReplacesFactory::previousCall {},
        Address::ZERO,
    );
    for vault in vaults {
        answers(&node, vault, IVault::stateCall {}, VaultState::Open);
        answers(&node, vault, IVault::timersCall {}, timers());
        answers(
            &node,
            vault,
            IVault::tankCall {},
            Tank {
                balance: U256::from(50_000_000u64),
                keepAt: U256::ZERO,
                usageSettled: U256::ZERO,
                usageAsOf: 0,
                lastCollection: start - 86_400,
                escapeRequestedAt: 0,
                capCarried: U256::ZERO,
                fillPending: false,
            },
        );
        answers(&node, vault, IStationVault::haltedAtCall {}, 0);
        answers(&node, vault, IVault::homeStationCall {}, STATION);
        answers(&node, vault, IVault::valuationCountCall {}, U256::ZERO);
        answers(&node, vault, IStationVault::unsettledCall {}, false);
        answers(&node, vault, IVault::claimCountCall {}, U256::ZERO);
    }
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    // Answers 404 to everything: not the executor.
    let executor = serve(axum::Router::new()).await;
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper_serving_none_for("executor-404", &rpc, &executor, 2);

    let mut said = 0;
    for _ in 0..100 {
        said = keeper.logged().matches("serves no total").count();
        if said >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(said, 2, "setup: the 404 was not said for each vault");
    let health = keeper
        .health_until(Duration::from_secs(15), |status| status != 200)
        .await;
    assert!(
        health
            .as_ref()
            .is_some_and(|(_, said)| said.contains("executor serves no vault")),
        "the executor URL serves no total for any vault, so no bill will ever be collected, \
         yet /health answered {health:?}"
    );
    assert!(
        keeper.logged().contains("answered 404 for every vault"),
        "the executor serving no vault is not said in the log"
    );
}
