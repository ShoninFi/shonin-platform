//! A dashboard shows a service's `/health` by its first line, cut to 60
//! characters. With three kinds of trouble at once (a chain out of reach, a
//! send not going out, a bill the vault refuses) the keeper's first line
//! still fits: it names the most serious and counts the rest, and every one
//! is said in full on the lines after it.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, a_spec, a_tank, answers, node, one_vault, serve, serve_node, start_keeper_on,
    timers,
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn the_health_line_a_dashboard_reads_says_what_is_wrong() {
    let node = node(1_700_000_000);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::tankCall {}, a_tank(5_000_000));
    answers(&node, VAULT, IStationVault::haltedAtCall {}, 0);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(
        &node,
        VAULT,
        IVault::valuationCountCall {},
        alloy::primitives::U256::ZERO,
    );
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(
        &node,
        VAULT,
        IVault::claimCountCall {},
        alloy::primitives::U256::ZERO,
    );
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    {
        let mut locked = node.lock().unwrap();
        locked.on_call = Some(Arc::new(|node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 10;
                return Some(Ok(IVault::stateCall::abi_encode_returns(&VaultState::Open)));
            }
            None
        }));
        // The vault refuses the bill's `keep`, and the keeper's account has
        // no gas money for anything else.
        locked.on_send = Some(Arc::new(|_, to, data| {
            if to == VAULT && data[..4] == IVault::keepCall::SELECTOR {
                return Some(json!({"code": 3, "message": "execution reverted: NotBlessed()"}));
            }
            Some(json!({"code": -32000, "message": "insufficient funds for gas * price + value"}))
        }));
    }
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(|| async {
            axum::Json(json!({
                "total": "1000000",
                "as_of": 1_699_999_990u64,
                "signature": "0x01",
            }))
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    // Hyperliquid's node is out of reach.
    let keeper = start_keeper_on(
        "health-line",
        &[
            ("base", 31337, &rpc),
            ("hyperliquid", 31348, "http://127.0.0.1:1"),
        ],
        &executor,
    );

    // Until all three are in the health.
    let mut last = None;
    for _ in 0..100 {
        last = keeper.health().await;
        if last.as_ref().is_some_and(|(_, said)| {
            said.contains("not kept")
                && said.contains("not going out")
                && said.contains("refuses to collect")
        }) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let (status, said) = last.expect("the keeper's health never answered");
    let first = said.lines().next().unwrap_or_default().to_string();
    assert_eq!(status, 503);
    assert!(
        said.contains("not kept")
            && said.contains("not going out")
            && said.contains("refuses to collect"),
        "the three troubles were not all in the health: {said:?}"
    );
    assert!(
        first.len() <= 60,
        "the keeper's first health line is {} characters, past the dashboard's 60: {first:?}",
        first.len()
    );
    assert_eq!(
        first, "keeper: 2 chains not kept; +2 more",
        "the first line does not name the most serious trouble and count the rest: {said:?}"
    );
}
