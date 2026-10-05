//! The keeper keeps the vaults of every factory the current one replaces.
//! When an earlier factory's `vaults()` can't be read (a node's `eth_call`
//! gas cap on an unpaginated list, or a node that refuses it), its vaults
//! are kept as it last listed them: they stay in the round, their standing
//! alarms stay up, and health says the factory's list is unreadable.
//!
//! Here an earlier factory's open vault refuses the `keep` carrying its bill
//! (`NotBlessed`), which shows in health as "bills refused". Then the earlier
//! factory's list stops being readable. Nothing about the vault has changed:
//! its bill is still uncollected and still tried, and health says both.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    FACTORY, IReplacesFactory, STATION, a_spec, a_tank, answers, called, node, serve, serve_node,
    start_keeper, timers, wait_until,
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replaced_factory_whose_vaults_cannot_be_listed_still_has_them_kept() {
    let start = 1_700_000_000u64;
    let node = node(start);
    let old_factory = Address::repeat_byte(0x67);
    let good = Address::repeat_byte(0x71);
    let old = Address::repeat_byte(0x72);
    answers(&node, FACTORY, IVaultFactory::vaultsCall {}, vec![good]);
    answers(
        &node,
        FACTORY,
        IReplacesFactory::previousCall {},
        old_factory,
    );
    answers(
        &node,
        old_factory,
        IReplacesFactory::previousCall {},
        Address::ZERO,
    );
    for vault in [good, old] {
        answers(&node, vault, IVault::stateCall {}, VaultState::Open);
        answers(&node, vault, IVault::timersCall {}, timers());
        answers(&node, vault, IStationVault::haltedAtCall {}, 0);
        answers(&node, vault, IVault::homeStationCall {}, STATION);
        answers(&node, vault, IVault::valuationCountCall {}, U256::ZERO);
        answers(&node, vault, IStationVault::unsettledCall {}, false);
        answers(&node, vault, IVault::claimCountCall {}, U256::ZERO);
    }
    answers(&node, good, IVault::tankCall {}, a_tank(0));
    // The old vault has a tank, so its bill is asked for and sent.
    answers(&node, old, IVault::tankCall {}, a_tank(50_000_000));
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("old_listed", 1);
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == good && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 10;
            }
            if to == old_factory && data[..4] == IVaultFactory::vaultsCall::SELECTOR {
                return Some(if node.vars["old_listed"] == 1 {
                    Ok(IVaultFactory::vaultsCall::abi_encode_returns(&vec![old]))
                } else {
                    // Too many vaults for the node's `eth_call` gas cap.
                    Err("out of gas".to_string())
                });
            }
            None
        }));
        // The old vault refuses every keep carrying a total, on its merits.
        locked.on_send = Some(Arc::new(move |_, to, data| {
            (to == old
                && IVault::keepCall::abi_decode(data).is_ok_and(|keep| !keep.total.is_zero()))
            .then(|| json!({"code": 3, "message": "execution reverted: NotBlessed()"}))
        }));
    }
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || async move {
            axum::Json(json!({
                "total": "1000000",
                "as_of": start - 1,
                "signature": "0x01",
            }))
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("old-factory-unlisted", &rpc, &executor);

    let refused = keeper
        .health_until(Duration::from_secs(60), |status| status == 503)
        .await;
    assert!(
        refused
            .as_ref()
            .is_some_and(|(status, said)| *status == 503 && said.contains("bills refused")),
        "setup: the old factory's vault's refused bill never showed in health: {refused:?}"
    );

    // The old factory's list can no longer be read.
    node.lock().unwrap().vars.insert("old_listed", 0);
    let rounds_then = called::<IVault::stateCall>(&node, good);
    let old_rounds_then = called::<IVault::stateCall>(&node, old);
    assert!(
        wait_until(Duration::from_secs(60), || {
            called::<IVault::stateCall>(&node, good) >= rounds_then + 6
        })
        .await,
        "setup: the keeper did not run six more rounds"
    );
    let health = keeper.health().await;
    let old_rounds = called::<IVault::stateCall>(&node, old) - old_rounds_then;
    assert!(
        old_rounds >= 5,
        "an earlier factory's vault dropped out of the rounds once its list couldn't be read: \
         looked at {old_rounds} times in six rounds"
    );
    assert!(
        health.as_ref().is_some_and(|(status, said)| *status == 503
            && said.contains("factory list unreadable")
            && said.contains("bills the vault refuses")),
        "an earlier factory's list can't be read and its vault's bill is still refused, and \
         /health answered {health:?}"
    );
    let warned = keeper
        .logged()
        .matches("could not read an earlier factory's vaults")
        .count();
    assert_eq!(
        warned, 1,
        "the unreadable list was not said once at warn, then left for ten minutes"
    );
}
