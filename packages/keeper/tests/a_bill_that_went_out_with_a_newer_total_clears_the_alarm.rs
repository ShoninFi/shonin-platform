//! A closed vault inside its last bill's window, its bill asked for once a
//! minute. The executor signs a new total each time it is asked. The node
//! turns the first `keep` away once (a gateway's rate limit); a minute later
//! the keeper sends `keep` with the executor's newer total, and that one
//! goes through.
//!
//! A send not going out is kept by the hash of the call itself, and the
//! first total's call is never sent again. The newer call for the same
//! purpose at the same contract going through takes the older one off: the
//! keeper's `/health` answers 200 again at once, not fifteen minutes later.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    STATION, VAULT, a_spec, answers, node, one_vault, sent, serve, serve_node, start_keeper,
    timers, wait_until,
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn a_bill_that_went_out_with_a_newer_total_clears_the_alarm() {
    let start = 1_700_000_000u64;
    let node = node(start);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(
        &node,
        VAULT,
        IVault::tankCall {},
        Tank {
            balance: U256::from(50_000_000u64),
            keepAt: U256::ZERO,
            usageSettled: U256::ZERO,
            usageAsOf: 0,
            lastCollection: start - 100,
            escapeRequestedAt: 0,
            capCarried: U256::ZERO,
            fillPending: false,
        },
    );
    answers(&node, VAULT, IStationVault::haltedAtCall {}, start - 100);
    answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::stoppedCall {}, true);
    answers(
        &node,
        STATION,
        IStation::valueCall {},
        IStation::valueReturn {
            mark: U256::ZERO,
            exit: U256::ZERO,
        },
    );
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        locked.vars.insert("turned_away", 0);
        locked.on_call = Some(Arc::new(|node, to, data| {
            // Half a chain-minute a round.
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 30;
                *node.vars.get_mut("rounds").unwrap() += 1;
                return Some(Ok(IVault::stateCall::abi_encode_returns(
                    &VaultState::Closed,
                )));
            }
            None
        }));
        locked.on_send = Some(Arc::new(|node, to, data| {
            if to == VAULT
                && data[..4] == IVault::keepCall::SELECTOR
                && node.vars["turned_away"] == 0
            {
                node.vars.insert("turned_away", 1);
                return Some(json!({"code": -32005, "message": "request rate limited"}));
            }
            None
        }));
    }
    // The executor signs a new total, as of the chain's head, each time.
    let asked_of = node.clone();
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || {
            let node = asked_of.clone();
            async move {
                let now = node.lock().unwrap().timestamp;
                axum::Json(json!({
                    "total": (now - 1_699_000_000).to_string(),
                    "as_of": now,
                    "signature": format!("0x{}", "11".repeat(65)),
                }))
            }
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("newer-bill", &rpc, &executor);

    // The first keep was turned away, and a later one went through.
    assert!(
        wait_until(Duration::from_secs(40), || {
            node.lock().unwrap().vars["turned_away"] == 1
                && sent::<IVault::keepCall>(&node, VAULT) >= 1
        })
        .await,
        "no keep went through after the first was turned away"
    );
    let went_through_at = node.lock().unwrap().vars["rounds"];
    // Three more rounds, a chain-minute and a half, every send going out.
    assert!(
        wait_until(Duration::from_secs(20), || {
            node.lock().unwrap().vars["rounds"] >= went_through_at + 3
        })
        .await
    );
    let health = keeper.health().await;
    assert_eq!(
        health.as_ref().map(|(status, _)| *status),
        Some(200),
        "the vault's bill went out with a newer total {} rounds ago and every send since has \
         gone out, yet /health answered {health:?}",
        node.lock().unwrap().vars["rounds"] - went_through_at
    );
}
