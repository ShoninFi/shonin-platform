//! A halted vault inside its grace, whose last valuation found its station
//! empty, so `close` would go through. The executor answers at once with a
//! newer total. The node turns the first `keep` away (a gateway's rate
//! limit) and takes everything else.
//!
//! `close` has no window, and once the vault is closed its author may take
//! the tank at once. So `close` waits for a round in which the bill it was
//! held for went through: the `keep` goes first, and only then the close.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    STATION, VAULT, a_spec, answers, name, node, one_vault, serve, serve_node, start_keeper,
    timers, wait_until,
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_halted_vault_is_not_closed_before_its_bill_goes_through() {
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
    // The vault would close.
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
        locked.vars.insert("keeps_turned_away", 0);
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
            if to == VAULT
                && data[..4] == IVault::keepCall::SELECTOR
                && node.vars["keeps_turned_away"] == 0
            {
                *node.vars.get_mut("keeps_turned_away").unwrap() += 1;
                return Some(json!({"code": -32005, "message": "rate limited"}));
            }
            if to == VAULT && data[..4] == IVault::closeCall::SELECTOR {
                node.vars.insert("closed", 1);
            }
            None
        }));
    }
    // The executor answers at once with a total newer than anything
    // collected.
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
    let rpc = serve_node(node.clone()).await;
    let _keeper = start_keeper("close-after-bill", &rpc, &executor);

    assert!(
        wait_until(Duration::from_secs(60), || {
            let locked = node.lock().unwrap();
            locked.vars["closed"] == 1 && locked.vars["rounds"] >= 4
        })
        .await,
        "the vault was never closed"
    );
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
    let first_close = order.iter().position(|what| *what == "close");
    let first_keep = order.iter().position(|what| *what == "keep");
    assert!(
        first_keep.is_some() && first_keep < first_close,
        "the halted vault was closed (its author may then take the tank at once) before the \
         bill the keeper had asked for went through; sent to the vault, in order: {order:?}"
    );
}
