//! A cancelled vault whose station still has a key approved, its blessing
//! gone, so the keeper means to revoke it; but the node's estimate of the
//! revoke reverts. The keeper's account is one the node signs for, and such
//! a node, left to fill the gas in itself, sends a call whose estimate
//! reverts anyway at the block's whole gas limit. The keeper asks the
//! estimate first: the revoke is asked again each round, and never sent.

mod process;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::Address;
use alloy::sol_types::SolCall;
use chain::bindings::station::IStation;
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, a_registry, a_spec, answers, answers_exactly, called, count_estimates, name,
    node, one_vault, sent, serve_node, start_keeper, timers, wait_until,
};

#[tokio::test(flavor = "multi_thread")]
async fn a_send_whose_estimate_reverts_is_never_sent() {
    let node = node(1_700_000_005);
    one_vault(&node);
    let local = name("local");
    let key = Address::repeat_byte(0x77);
    answers(&node, VAULT, IVault::stateCall {}, VaultState::Cancelled);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![local]));
    answers_exactly(
        &node,
        STATION,
        IStation::approvedKeyCall { driver: local },
        key,
    );
    a_registry(&node, local, key, false);
    let estimated = Arc::new(Mutex::new(0));
    count_estimates(
        &node,
        STATION,
        IStation::revokeKeyCall::SELECTOR,
        estimated.clone(),
        Some("KeyStillStands()"),
    );
    let rpc = serve_node(node.clone()).await;
    let _keeper = start_keeper("estimate-reverts", &rpc, "http://127.0.0.1:1");

    // Four rounds.
    assert!(
        wait_until(Duration::from_secs(30), || {
            called::<IVault::stateCall>(&node, VAULT) >= 4
        })
        .await
    );
    assert_eq!(
        sent::<IStation::revokeKeyCall>(&node, STATION),
        0,
        "a revoke whose estimate reverts was sent"
    );
    let asked = *estimated.lock().unwrap();
    assert!(
        asked >= 3,
        "the keeper asked the revoke's estimate {asked} times in four rounds"
    );
}
