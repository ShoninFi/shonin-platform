//! A halted vault inside its grace, whose last valuation found its station
//! empty, so `close` would go through. The executor answers at once with a
//! newer total. The first `keep` carrying it is mined and reverts on chain
//! (its estimate went through; something changed between the estimate and
//! the block, or it ran out of the estimate's gas). Everything else goes
//! through.
//!
//! A `keep` mined and reverted collected nothing: the vault's `usageSettled`
//! hasn't moved. So the total is not taken as sent, the close waits, and it
//! goes only once a later `keep` carrying the bill has gone through.

mod process;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use axum::Json;
use axum::extract::State;
use axum::routing::post;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    STATION, VAULT, a_spec, answers, name, node, one_vault, serve, serve_node, start_keeper,
    timers, wait_until,
};
use serde_json::{Value, json};

/// A node in front of the scripted one whose receipt for the first `keep`
/// carrying a total says it reverted.
#[derive(Clone)]
struct Reverting {
    behind: String,
    hashes: Arc<Mutex<HashSet<String>>>,
    reverted: Arc<Mutex<usize>>,
}

async fn one(reverting: &Reverting, asked: &Value) -> Value {
    let answer: Value = reqwest::Client::new()
        .post(&reverting.behind)
        .json(asked)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let method = asked["method"].as_str().unwrap_or_default();
    if method == "eth_sendTransaction" {
        let tx = &asked["params"][0];
        let data = tx
            .get("input")
            .or_else(|| tx.get("data"))
            .and_then(Value::as_str)
            .unwrap_or("0x");
        let data = alloy::hex::decode(data).unwrap();
        let is_bill = data.len() >= 4
            && data[..4] == IVault::keepCall::SELECTOR
            && IVault::keepCall::abi_decode(&data).is_ok_and(|keep| !keep.total.is_zero());
        let mut reverted = reverting.reverted.lock().unwrap();
        if is_bill
            && *reverted == 0
            && let Some(hash) = answer["result"].as_str()
        {
            *reverted += 1;
            reverting.hashes.lock().unwrap().insert(hash.to_string());
        }
    }
    if method == "eth_getTransactionReceipt"
        && let Some(hash) = asked["params"][0].as_str()
        && reverting.hashes.lock().unwrap().contains(hash)
    {
        let mut answer = answer;
        answer["result"]["status"] = json!("0x0");
        return answer;
    }
    answer
}

async fn in_front(behind: String) -> (String, Arc<Mutex<usize>>) {
    let reverting = Reverting {
        behind,
        hashes: Default::default(),
        reverted: Default::default(),
    };
    let count = reverting.reverted.clone();
    let app = axum::Router::new()
        .route(
            "/",
            post(
                |State(reverting): State<Reverting>, Json(body): Json<Value>| async move {
                    Json(match &body {
                        Value::Array(batch) => {
                            let mut answers = Vec::new();
                            for asked in batch {
                                answers.push(one(&reverting, asked).await);
                            }
                            Value::Array(answers)
                        }
                        asked => one(&reverting, asked).await,
                    })
                },
            ),
        )
        .with_state(reverting);
    (serve(app).await, count)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_keep_that_reverted_on_chain_does_not_let_the_close_go() {
    // Between slots' windows, so no valuation is due and nothing else is sent.
    let start = 1_700_000_005u64;
    let halted_at = start - 100;
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
            lastCollection: halted_at - 3_600,
            escapeRequestedAt: 0,
            capCarried: U256::ZERO,
            fillPending: false,
        },
    );
    answers(&node, VAULT, IStationVault::haltedAtCall {}, halted_at);
    answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::closeCall {}, IVault::closeReturn {});
    answers(
        &node,
        STATION,
        IStation::specCall {},
        a_spec(vec![name("local")]),
    );
    answers(
        &node,
        STATION,
        IStation::approvedKeyCall {
            driver: name("local"),
        },
        Address::ZERO,
    );
    answers(&node, STATION, IStation::stoppedCall {}, false);
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        locked.vars.insert("closed", 0);
        locked.on_call = Some(Arc::new(|node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                *node.vars.get_mut("rounds").unwrap() += 1;
                let state = if node.vars["closed"] == 1 {
                    VaultState::Closed
                } else {
                    VaultState::Killed
                };
                return Some(Ok(IVault::stateCall::abi_encode_returns(&state)));
            }
            None
        }));
        locked.on_send = Some(Arc::new(|node, to, data| {
            if to == VAULT && data[..4] == IVault::closeCall::SELECTOR {
                node.vars.insert("closed", 1);
            }
            None
        }));
    }
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || async move {
            axum::Json(json!({
                "total": "1000000",
                "as_of": start - 1,
                "signature": format!("0x{}", "11".repeat(65)),
            }))
        }),
    ))
    .await;
    let behind = serve_node(node.clone()).await;
    let (rpc, reverted) = in_front(behind).await;
    let _keeper = start_keeper("reverted-keep", &rpc, &executor);

    // Rounds enough for the keep, its revert, and whatever follows.
    wait_until(Duration::from_secs(60), || {
        node.lock().unwrap().vars["rounds"] >= 8
    })
    .await;
    assert_eq!(*reverted.lock().unwrap(), 1, "no keep was sent to revert");

    let order: Vec<&'static str> = node
        .lock()
        .unwrap()
        .sent
        .iter()
        .filter(|(to, data)| *to == VAULT && data.len() >= 4)
        .filter_map(|(_, data)| {
            if data[..4] == IVault::keepCall::SELECTOR {
                Some("keep")
            } else if data[..4] == IVault::closeCall::SELECTOR {
                Some("close")
            } else {
                None
            }
        })
        .collect();
    // The first keep reverted; only a later one can have gone through.
    let first_close = order.iter().position(|what| *what == "close");
    let good_keep = order
        .iter()
        .enumerate()
        .filter(|(_, what)| **what == "keep")
        .nth(1)
        .map(|(at, _)| at);
    assert!(
        first_close.is_none() || good_keep.is_some_and(|keep| Some(keep) < first_close),
        "the halted vault was closed (its author may take the tank at once) though the only \
         keep carrying its bill reverted on chain and none went through; sent to the vault, in \
         order (the first keep reverted): {order:?}"
    );
}
