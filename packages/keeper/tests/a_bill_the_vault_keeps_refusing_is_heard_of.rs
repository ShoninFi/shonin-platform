//! One open vault with a tank and a bill. The executor answers with a newer
//! total, and the vault refuses every `keep` carrying it (its gas estimate
//! reverts, `NotBlessed`: the executor signs with a key the vault doesn't
//! take, say). A bill never collected breaks the daily bound on what a vault
//! owes.
//!
//! The keeper tries the `keep` again each round (an estimate costs
//! nothing), warns once (and at most every ten minutes after), and answers
//! `/health` with 503 naming the refused bill. Once the vault takes the
//! `keep`, the keeper is up again.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, a_spec, a_tank, answers, node, one_vault, serve, serve_node, start_keeper,
    timers, wait_until,
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_bill_the_vault_keeps_refusing_is_heard_of() {
    let node = node(1_700_000_005);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::tankCall {}, a_tank(5_000_000));
    answers(&node, VAULT, IStationVault::haltedAtCall {}, 0);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        locked.vars.insert("refused", 0);
        locked.vars.insert("refusing", 1);
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 61;
                *node.vars.get_mut("rounds").unwrap() += 1;
                return Some(Ok(IVault::stateCall::abi_encode_returns(&VaultState::Open)));
            }
            None
        }));
        // Every `keep` with a total is refused on its merits, while the vault
        // is refusing.
        locked.on_send = Some(Arc::new(|node, to, data| {
            if to == VAULT
                && node.vars["refusing"] == 1
                && data[..4] == IVault::keepCall::SELECTOR
                && IVault::keepCall::abi_decode(data).is_ok_and(|keep| !keep.total.is_zero())
            {
                *node.vars.get_mut("refused").unwrap() += 1;
                return Some(json!({"code": 3, "message": "execution reverted: NotBlessed()"}));
            }
            None
        }));
    }
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(|| async {
            axum::Json(json!({
                "total": "1000000",
                "as_of": 1_700_000_001u64,
                "signature": "0x01",
            }))
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("keep-refused", &rpc, &executor);

    assert!(
        wait_until(Duration::from_secs(120), || {
            let locked = node.lock().unwrap();
            locked.vars["rounds"] >= 12 && locked.vars["refused"] >= 5
        })
        .await,
        "the keeper did not try the bill five times in twelve rounds"
    );
    let refused = node.lock().unwrap().vars["refused"];
    let health = keeper
        .health_until(Duration::from_secs(5), |status| status != 200)
        .await;
    let warned = keeper
        .logged()
        .lines()
        .filter(|line| line.contains("WARN") && line.contains("refuses the keep"))
        .count();
    assert!(
        warned == 1
            && health.as_ref().is_some_and(|(status, said)| {
                *status == 503
                    && said.contains("bills refused")
                    && said.contains(&format!("{VAULT:?}"))
            }),
        "the vault refused the bill's keep {refused} times: warned {warned} times (once is \
         wanted), health {health:?}"
    );

    // The vault takes it: the bill is collected and the keeper is up.
    node.lock().unwrap().vars.insert("refusing", 0);
    let health = keeper
        .health_until(Duration::from_secs(30), |status| status == 200)
        .await;
    assert!(
        health.as_ref().is_some_and(|(status, _)| *status == 200),
        "the bill went through and the keeper still says it is refused: {health:?}"
    );
}
