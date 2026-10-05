//! Two dozen open vaults and one more, V, listed last. The executor answers
//! every ask in four seconds. In the keeper's first round every vault's
//! daily ask goes out in the daily lane, eight at a time, so V's waits about
//! twelve seconds for its turn. V then halts, inside its grace, and its last
//! valuation found its station empty, so `close` would go through.
//!
//! Halted and closed vaults' asks don't queue behind open vaults', and V's
//! own daily ask, still queued, doesn't hold back its halted ask either: the
//! halted ask goes out in its own lane, the close waits for its answer, and
//! the bill is collected before V is closed.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    FACTORY, STATION, a_spec, answers, name, node, serve, serve_node, start_keeper, timers,
    wait_until,
};
use serde_json::json;

alloy::sol! {
    interface IReplacesFactory {
        function previous() external view returns (address);
    }
}

const OTHERS: u8 = 24;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_halted_vault_s_ask_does_not_wait_behind_its_own_daily_ask() {
    // Between slots' windows, so no valuation is due.
    let start = 1_700_000_005u64;
    let halted_at = start - 100;
    let v = Address::repeat_byte(0xee);
    let mut vaults: Vec<Address> = (0..OTHERS)
        .map(|n| Address::repeat_byte(0x30 + n))
        .collect();
    vaults.push(v);
    let node = node(start);
    answers(&node, FACTORY, IVaultFactory::vaultsCall {}, vaults.clone());
    answers(
        &node,
        FACTORY,
        IReplacesFactory::previousCall {},
        Address::ZERO,
    );
    let tank = Tank {
        balance: U256::from(50_000_000u64),
        keepAt: U256::ZERO,
        usageSettled: U256::ZERO,
        usageAsOf: 0,
        lastCollection: halted_at - 3_600,
        escapeRequestedAt: 0,
        capCarried: U256::ZERO,
        fillPending: false,
    };
    for &vault in &vaults {
        answers(&node, vault, IVault::timersCall {}, timers());
        answers(&node, vault, IVault::tankCall {}, tank.clone());
        answers(&node, vault, IVault::homeStationCall {}, STATION);
        answers(&node, vault, IStationVault::unsettledCall {}, false);
        answers(&node, vault, IVault::valuationCountCall {}, U256::ZERO);
        answers(&node, vault, IVault::claimCountCall {}, U256::ZERO);
        answers(&node, vault, IVault::requestCountCall {}, U256::ZERO);
        answers(&node, vault, IStationVault::haltGraceCall {}, 600);
        if vault != v {
            answers(&node, vault, IStationVault::haltedAtCall {}, 0);
            answers(&node, vault, IVault::stateCall {}, VaultState::Open);
        }
    }
    // V would close.
    answers(&node, v, IVault::closeCall {}, IVault::closeReturn {});
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
        locked.vars.insert("v_rounds", 0);
        locked.vars.insert("closed", 0);
        // V is open in the keeper's first round, and halted after.
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to != v {
                return None;
            }
            if data[..4] == IVault::stateCall::SELECTOR {
                *node.vars.get_mut("v_rounds").unwrap() += 1;
                let state = if node.vars["closed"] == 1 {
                    VaultState::Closed
                } else if node.vars["v_rounds"] == 1 {
                    VaultState::Open
                } else {
                    VaultState::Killed
                };
                return Some(Ok(IVault::stateCall::abi_encode_returns(&state)));
            }
            if data[..4] == IStationVault::haltedAtCall::SELECTOR {
                let at = if node.vars["v_rounds"] <= 1 {
                    0
                } else {
                    halted_at
                };
                return Some(Ok(IStationVault::haltedAtCall::abi_encode_returns(&at)));
            }
            None
        }));
        locked.on_send = Some(Arc::new(move |node, to, data| {
            if to == v && data[..4] == IVault::closeCall::SELECTOR {
                node.vars.insert("closed", 1);
            }
            None
        }));
    }
    // A busy executor: every answer takes four seconds, inside its five.
    let executor = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || async move {
            tokio::time::sleep(Duration::from_secs(4)).await;
            axum::Json(json!({
                "total": "1000000",
                "as_of": start - 1,
                "signature": format!("0x{}", "11".repeat(65)),
            }))
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    let _keeper = start_keeper("own-daily-ask", &rpc, &executor);

    assert!(
        wait_until(Duration::from_secs(90), || {
            let locked = node.lock().unwrap();
            locked.vars["closed"] == 1 && locked.vars["v_rounds"] >= 6
        })
        .await,
        "V was never closed"
    );
    let order: Vec<&'static str> = node
        .lock()
        .unwrap()
        .sent
        .iter()
        .filter(|(to, data)| *to == v && data.len() >= 4)
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
        "V was closed (its author may take the tank at once) before its bill was collected: its \
         halted ask was held behind its own daily ask, queued behind the open vaults' asks; \
         sent to V, in order: {order:?}"
    );
}
