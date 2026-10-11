//! Two open vaults on one chain. The second answers every read but
//! `timers()`, which never decodes (a vault of an earlier factory whose
//! `Timers` differ from the keeper's, say, or one whose read reverts). Its
//! round stops at that read every time: it is never valued, settled, billed
//! or tried for claims.
//!
//! The chain's rounds still finish, but the vault is heard of: in the
//! keeper's health once its rounds have not finished three times in a row,
//! and at warn once, then at most every ten minutes, not every round.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    FACTORY, IReplacesFactory, STATION, a_spec, a_tank, answers, called, node, serve_node,
    start_keeper, timers, wait_until,
};

#[tokio::test(flavor = "multi_thread")]
async fn a_vault_the_keeper_cannot_read_at_all_is_heard_of() {
    let node = node(1_700_000_000);
    let good = Address::repeat_byte(0x71);
    let unreadable = Address::repeat_byte(0x72);
    answers(
        &node,
        FACTORY,
        IVaultFactory::vaultsCall {},
        vec![good, unreadable],
    );
    answers(
        &node,
        FACTORY,
        IReplacesFactory::previousCall {},
        Address::ZERO,
    );
    for vault in [good, unreadable] {
        answers(&node, vault, IVault::stateCall {}, VaultState::Open);
        answers(&node, vault, IVault::tankCall {}, a_tank(0));
        answers(&node, vault, IStationVault::haltedAtCall {}, 0);
        answers(&node, vault, IVault::homeStationCall {}, STATION);
        answers(&node, vault, IVault::valuationCountCall {}, U256::ZERO);
        answers(&node, vault, IStationVault::unsettledCall {}, false);
        answers(&node, vault, IVault::claimCountCall {}, U256::ZERO);
    }
    answers(&node, good, IVault::timersCall {}, timers());
    // Both pass the initial policy check. The second becomes unreadable
    // after the first ordinary vault round begins.
    answers(&node, unreadable, IVault::timersCall {}, timers());
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    node.lock().unwrap().on_call = Some(Arc::new(move |node, to, data| {
        if to == good && data[..4] == IVault::stateCall::SELECTOR {
            node.timestamp += 10;
            node.by_selector
                .remove(&(unreadable, IVault::timersCall::SELECTOR));
        }
        None
    }));
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("unreadable-vault", &rpc, "http://127.0.0.1:1");

    assert!(
        wait_until(Duration::from_secs(60), || {
            called::<IVault::stateCall>(&node, good) >= 12
        })
        .await,
        "the keeper did not run twelve rounds"
    );
    let rounds = called::<IVault::stateCall>(&node, good);
    let warned = keeper
        .logged()
        .matches("the vault's round did not finish")
        .count();
    let health = keeper.health().await;
    assert!(
        health.as_ref().is_some_and(|(status, said)| *status == 503
            && said.contains("1 vaults not kept")
            && said.contains(&format!("{unreadable:?}")))
            && warned == 1,
        "in {rounds} rounds one vault was never kept at all; /health answered {health:?}, and \
         its round was said not to finish {warned} times"
    );
}
