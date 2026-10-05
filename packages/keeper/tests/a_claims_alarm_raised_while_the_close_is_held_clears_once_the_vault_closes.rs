//! A halted vault past its grace, its stopped station empty and a
//! valuation after the halt having found it so, so `close` would go through.
//! One claim is still waiting. The vault refuses every try of it before it
//! goes out (`settleClaim` reverts `WrongState` on a vault ready to close),
//! and the vault refuses the `keep` carrying its bill (the executor's key's
//! blessing lapsed, say), so the close is held for the bill.
//!
//! While the close is held the keeper tries the claim each slot, each try is
//! refused, and after ten slots the keeper says "claims refused" in its
//! health. That is fair while it lasts. Then the bill goes through, the vault
//! closes, and the claim is paid out of the closed vault (`claimClosed`). The
//! keeper never tries the claim again, so nothing ever notes a claim's try
//! going through: the alarm must still clear, since there is nothing left
//! the vault refuses. A keeper whose health says 503 for good about a vault
//! that is closed and paid out is a false alarm that never clears.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{Claim, ClaimState, IVault, Tank, VaultState};
use process::{
    DAY, STATION, VAULT, WAD, a_spec, answers, node, one_vault, serve, serve_node, start_keeper,
    timers, wait_until,
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_claims_alarm_raised_while_the_close_is_held_clears_once_the_vault_closes() {
    // Five seconds into a slot, and each round moves the chain a slot on, so
    // no round lands in a valuation window and nothing is valued.
    let start = 1_700_000_005u64;
    let halted_at = start - 3 * DAY;
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
            lastCollection: halted_at,
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
    answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::from(1u64));
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::stoppedCall {}, true);
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    answers(
        &node,
        STATION,
        IStation::valueCall {},
        IStation::valueReturn {
            mark: U256::ZERO,
            exit: U256::ZERO,
        },
    );
    answers(
        &node,
        STATION,
        IStation::unwindCall {
            fraction: U256::from(WAD),
        },
        IStation::unwindReturn {
            sentHome: U256::ZERO,
            blocked: false,
        },
    );
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        locked.vars.insert("closed", 0);
        locked.vars.insert("claim_paid", 0);
        locked.vars.insert("claim_refused", 0);
        locked.vars.insert("bill_refusing", 1);
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 10;
                *node.vars.get_mut("rounds").unwrap() += 1;
                let state = if node.vars["closed"] == 1 {
                    VaultState::Closed
                } else {
                    VaultState::Killed
                };
                return Some(Ok(IVault::stateCall::abi_encode_returns(&state)));
            }
            if to == VAULT && data[..4] == IVault::closeCall::SELECTOR {
                return Some(if node.vars["closed"] == 1 {
                    Err("WrongState()".to_string())
                } else {
                    Ok(Vec::new())
                });
            }
            if to == VAULT && data[..4] == IVault::claimAtCall::SELECTOR {
                let state = if node.vars["claim_paid"] == 1 {
                    ClaimState::Paid
                } else {
                    ClaimState::Waiting
                };
                return Some(Ok(IVault::claimAtCall::abi_encode_returns(&Claim {
                    owner: Address::repeat_byte(0x44),
                    shares: U256::from(1_000_000u64),
                    bookedAt: halted_at - DAY,
                    lastAttemptAt: halted_at - DAY,
                    state,
                    paid: U256::ZERO,
                })));
            }
            None
        }));
        locked.on_send = Some(Arc::new(|node, to, data| {
            if to != VAULT {
                return None;
            }
            // A vault ready to close refuses a claim's try.
            if data[..4] == IVault::settleClaimCall::SELECTOR {
                *node.vars.get_mut("claim_refused").unwrap() += 1;
                return Some(json!({"code": 3, "message": "execution reverted: WrongState()"}));
            }
            if data[..4] == IVault::keepCall::SELECTOR
                && node.vars["bill_refusing"] == 1
                && IVault::keepCall::abi_decode(data).is_ok_and(|keep| !keep.total.is_zero())
            {
                return Some(json!({"code": 3, "message": "execution reverted: NotBlessed()"}));
            }
            if data[..4] == IVault::closeCall::SELECTOR {
                node.vars.insert("closed", 1);
            }
            if data[..4] == IVault::claimClosedCall::SELECTOR {
                node.vars.insert("claim_paid", 1);
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
                "signature": "0x01",
            }))
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("claims-alarm-after-close", &rpc, &executor);

    // While the close is held, the claim is refused slot after slot, and
    // that is heard of.
    assert!(
        wait_until(Duration::from_secs(120), || {
            node.lock().unwrap().vars["claim_refused"] >= 12
        })
        .await,
        "the claim was not tried slot after slot while the close was held"
    );
    let health = keeper
        .health_until(Duration::from_secs(10), |status| status != 200)
        .await;
    assert!(
        health.as_ref().is_some_and(
            |(status, said)| *status == 503 && said.contains("claims the vault refuses")
        ),
        "setup: the refused claims were not heard of: {health:?}"
    );

    // The bill goes through; the vault closes and its claim is paid.
    node.lock().unwrap().vars.insert("bill_refusing", 0);
    assert!(
        wait_until(Duration::from_secs(60), || {
            let locked = node.lock().unwrap();
            locked.vars["closed"] == 1 && locked.vars["claim_paid"] == 1
        })
        .await,
        "setup: the vault never closed and paid its claim"
    );

    // Nothing is refused any more: the keeper is up again.
    let health = keeper
        .health_until(Duration::from_secs(60), |status| status == 200)
        .await;
    assert!(
        health.as_ref().is_some_and(|(status, _)| *status == 200),
        "the vault is closed and its claim paid, and the keeper's health still says the vault \
         refuses claims, a minute on (no try of a claim is ever made again to clear it): \
         {health:?}"
    );
}
