//! A cancelled vault whose station still has two keys approved. Driver A's
//! key has lapsed, so its `revokeKey` would go through, but the keeper's
//! account can't pay for gas and the node turns it away ("insufficient
//! funds"): a send not going out. Driver B's key still stands, so its
//! `revokeKey` is refused on its merits. The two are the same purpose at
//! the same contract, and different calls: B's refusal takes nothing of A's
//! off, so the keeper's health answers that a send isn't going out between
//! every round, and the node's reason is said once, not at every round.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::Address;
use alloy::sol_types::SolCall;
use chain::bindings::station::IStation;
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, a_spec, answers, answers_exactly, called, name, node, one_vault, serve_node,
    start_keeper, timers, wait_until,
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn a_call_refused_on_its_merits_does_not_hide_another_that_is_not_going_out() {
    let node = node(1_700_000_005);
    one_vault(&node);
    let (a, b) = (name("a"), name("b"));
    let (key_a, key_b) = (Address::repeat_byte(0xa1), Address::repeat_byte(0xb1));
    answers(&node, VAULT, IVault::stateCall {}, VaultState::Cancelled);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![a, b]));
    answers_exactly(
        &node,
        STATION,
        IStation::approvedKeyCall { driver: a },
        key_a,
    );
    answers_exactly(
        &node,
        STATION,
        IStation::approvedKeyCall { driver: b },
        key_b,
    );
    let revoke_a = IStation::revokeKeyCall {
        driver: a,
        key: key_a,
    }
    .abi_encode();
    let revoke_b = IStation::revokeKeyCall {
        driver: b,
        key: key_b,
    }
    .abi_encode();
    node.lock().unwrap().on_send = Some(Arc::new(move |_, to, data| {
        if to == STATION && data == revoke_a.as_slice() {
            return Some(
                json!({"code": -32000, "message": "insufficient funds for gas * price + value: balance 0"}),
            );
        }
        if to == STATION && data == revoke_b.as_slice() {
            return Some(json!({"code": 3, "message": "execution reverted: KeyStillStands"}));
        }
        None
    }));
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("sibling-refused", &rpc, "http://127.0.0.1:1");

    // Four rounds.
    assert!(
        wait_until(Duration::from_secs(30), || {
            called::<IVault::stateCall>(&node, VAULT) >= 4
        })
        .await
    );
    // Between rounds, as a dashboard or a supervisor polls it.
    let mut answered = Vec::new();
    for _ in 0..5 {
        answered.push(keeper.health().await.map(|(status, _)| status));
        tokio::time::sleep(Duration::from_millis(700)).await;
    }
    let warned = keeper
        .logged()
        .matches("the keeper's send did not go out")
        .count();
    let rounds = called::<IVault::stateCall>(&node, VAULT);
    assert!(
        answered.iter().all(|status| *status == Some(503)),
        "driver A's revoke has never gone out, yet /health answered {answered:?}"
    );
    assert_eq!(
        warned, 1,
        "the node turning the keeper's sends away was said {warned} times in {rounds} rounds"
    );
}
