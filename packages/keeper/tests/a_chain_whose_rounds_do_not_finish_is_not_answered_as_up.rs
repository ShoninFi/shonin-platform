//! The node answers who it is, so the chain connects, and then every read of
//! the vault list fails: a rate-limited or broken RPC, a factory address
//! that is wrong. The keeper keeps nothing on that chain, round after round,
//! and sends nothing, so nothing is "not going out"; its health still
//! answers that the chain is not kept. That the round did not finish is said
//! once, not at every round.

mod process;

use std::time::Duration;

use chain::bindings::factory::IVaultFactory;
use process::{FACTORY, called, node, serve_node, start_keeper, wait_until};

#[tokio::test(flavor = "multi_thread")]
async fn a_chain_whose_rounds_do_not_finish_is_not_answered_as_up() {
    let node = node(1_700_000_005);
    // No factory scripted: `vaults()` reverts every round.
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("rounds-not-finishing", &rpc, "http://127.0.0.1:1");

    assert!(
        wait_until(Duration::from_secs(30), || {
            called::<IVaultFactory::vaultsCall>(&node, FACTORY) >= 3
        })
        .await,
        "the keeper never read the factory"
    );
    let health = keeper
        .health_until(Duration::from_secs(20), |status| status == 503)
        .await;
    let (status, said) = health.clone().unwrap_or_default();
    assert_eq!(
        status, 503,
        "every round failed to read the chain, yet /health answered {health:?}"
    );
    let first = said.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("keeper: ") && first.len() <= 60,
        "{said:?}"
    );
    assert!(said.contains("rounds not finishing"), "{said:?}");
    let failed = keeper.logged().matches("the round did not finish").count();
    let rounds = called::<IVaultFactory::vaultsCall>(&node, FACTORY);
    assert_eq!(
        failed, 1,
        "the rounds failing was said {failed} times in {rounds} rounds"
    );
}
