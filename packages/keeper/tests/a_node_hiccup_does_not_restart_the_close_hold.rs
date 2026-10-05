//! A halted vault inside its grace whose `close` would go through, and whose
//! bill the vault refuses for good (every `keep` carrying a total is refused
//! on its merits, `NotBlessed`: its key's blessing lapsed, say). The close
//! waits for the bill for at most fifteen minutes, on both clocks, from when
//! it was first held; then the bill is written off and the close goes.
//!
//! The node is a little flaky: in one round in four, the keeper's read of
//! whether `close` would go through gets a node error (`header not found`,
//! code -32000), not a revert. That leaves the hold as it is. The chain's
//! clock moves a minute a round and the keeper's own clock is let off, so
//! the hold is over after about fifteen rounds and the vault is closed.

mod process;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    STATION, VAULT, a_spec, answers, node, one_vault, serve, serve_node, start_keeper_holding,
    timers, wait_until,
};
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_node_hiccup_does_not_restart_the_close_hold() {
    let start = 1_700_000_005u64;
    let halted_at = start - 60;
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
    // A grace of a week: the whole test runs inside it.
    answers(&node, VAULT, IStationVault::haltGraceCall {}, 7 * 86_400u64);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::stoppedCall {}, false);
    answers(&node, STATION, IStation::outsideBandCall {}, false);

    let rounds = Arc::new(AtomicU64::new(0));
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("closed", 0);
        locked.vars.insert("refused", 0);
        let counted = rounds.clone();
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                // A minute of chain time a round.
                node.timestamp += 60;
                counted.fetch_add(1, Ordering::SeqCst);
                let state = if node.vars["closed"] == 1 {
                    VaultState::Closed
                } else {
                    VaultState::Killed
                };
                return Some(Ok(IVault::stateCall::abi_encode_returns(&state)));
            }
            if to == VAULT && data[..4] == IVault::closeCall::SELECTOR {
                // The close would go through, whenever the node answers.
                return Some(if node.vars["closed"] == 0 {
                    Ok(Vec::new())
                } else {
                    Err("Closed()".to_string())
                });
            }
            None
        }));
        // The vault refuses every keep carrying a total, on its merits.
        locked.on_send = Some(Arc::new(|node, to, data| {
            if to != VAULT {
                return None;
            }
            if let Ok(keep) = IVault::keepCall::abi_decode(data)
                && !keep.total.is_zero()
            {
                *node.vars.get_mut("refused").unwrap() += 1;
                return Some(json!({"code": 3, "message": "execution reverted: NotBlessed()"}));
            }
            if data[..4] == IVault::closeCall::SELECTOR {
                node.vars.insert("closed", 1);
            }
            None
        }));
        // In one round in four, the node can't answer the read of `close`.
        let flaky = rounds.clone();
        locked.on_request = Some(Arc::new(move |method: &str, params: &Value| {
            if method != "eth_call" || !flaky.load(Ordering::SeqCst).is_multiple_of(4) {
                return None;
            }
            let tx = &params[0];
            let input = tx
                .get("input")
                .or_else(|| tx.get("data"))
                .and_then(Value::as_str)
                .unwrap_or("0x");
            let data = alloy::hex::decode(input).unwrap_or_default();
            (data.len() >= 4 && data[..4] == IVault::closeCall::SELECTOR)
                .then(|| json!({"code": -32000, "message": "header not found"}))
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
    let keeper = start_keeper_holding("hiccup-hold", &rpc, &executor, 0);

    // Fifteen minutes of chain time is fifteen rounds; sixty rounds is an
    // hour of it.
    let closed = wait_until(Duration::from_secs(240), || {
        node.lock().unwrap().vars["closed"] == 1 || rounds.load(Ordering::SeqCst) >= 60
    })
    .await
        && node.lock().unwrap().vars["closed"] == 1;
    let refused = node.lock().unwrap().vars["refused"];
    let warned = keeper
        .logged()
        .lines()
        .any(|line| line.contains("held the vault's close as long as it may"));
    assert!(
        refused > 0,
        "setup: the keeper never tried the bill ({} rounds)",
        rounds.load(Ordering::SeqCst)
    );
    assert!(
        closed,
        "after {} rounds (a minute of chain time each, the keeper's own hold let off) the \
         halted vault is still not closed: its bill was refused {refused} times, and the \
         fifteen-minute hold never ran out though the node failed now and then to answer \
         whether `close` would go through (written off and closed: {warned})",
        rounds.load(Ordering::SeqCst)
    );
}
