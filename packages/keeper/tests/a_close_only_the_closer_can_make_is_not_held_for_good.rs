//! A halted vault past its grace that only the Closer can close. Its
//! stopped station holds half a cent (5,000 dollar-units): more than the
//! vault's dust (a tenth of a minimum deposit of one cent, which the factory
//! allows), so a valuation never finds it empty and plain `close` never goes
//! through; and less than the cent the keeper's own unwind must bring home,
//! so the keeper never sends one. The Closer unwinds whatever the station
//! holds before it values, so `closeHalted` would go through, in every
//! slot's window. (A stranger's gift landed between the keeper's unwind and
//! its valuation each slot leaves the Closer the only way in the same way.)
//!
//! The vault refuses the bill for good (`NotBlessed`). The close waits for
//! its bill for at most fifteen minutes from when it was first held, then
//! the latest total gets one last try and the close goes. The hold starts on
//! the Closer's path, and each round's end finds plain `close` impossible:
//! that leaves the hold as it is, so its fifteen minutes run out and the
//! Closer is sent.

mod process;

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::closer::ICloser;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, Tank, VaultState};
use process::{
    STATION, VAULT, WAD, a_spec, answers, deployments, name, node, one_vault, serve, serve_node,
    timers, wait_until,
};
use serde_json::json;

const CLOSER: Address = Address::repeat_byte(0xc1);

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_close_only_the_closer_can_make_is_not_held_for_good() {
    // Each round at a slot's start: the next block lands inside the window.
    let start = 1_700_000_000u64;
    let halted_at = start - 2_000;
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
    answers(&node, VAULT, IStationVault::haltGraceCall {}, 600u64);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::requestCountCall {}, U256::ZERO);
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
    answers(&node, STATION, IStation::stoppedCall {}, true);
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    // Half a cent at the station, all of it sold home by an unwind.
    answers(
        &node,
        STATION,
        IStation::valueCall {},
        IStation::valueReturn {
            mark: U256::from(5_000u64),
            exit: U256::from(5_000u64),
        },
    );
    answers(
        &node,
        STATION,
        IStation::unwindCall {
            fraction: U256::from(WAD),
        },
        IStation::unwindReturn {
            sentHome: U256::from(5_000u64),
            blocked: false,
        },
    );
    // The Closer would close it in one go.
    answers(
        &node,
        CLOSER,
        ICloser::closeHaltedCall { vault: VAULT },
        ICloser::closeHaltedReturn {},
    );
    let rounds = Arc::new(AtomicU64::new(0));
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("closed", 0);
        locked.vars.insert("refused", 0);
        let counted = rounds.clone();
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                // A minute of chain time a round, each at a slot's start.
                node.timestamp += 60;
                counted.fetch_add(1, Ordering::SeqCst);
                let state = if node.vars["closed"] == 1 {
                    VaultState::Closed
                } else {
                    VaultState::Killed
                };
                return Some(Ok(IVault::stateCall::abi_encode_returns(&state)));
            }
            // A valuation never finds half a cent empty: plain close never
            // goes through.
            if to == VAULT && data[..4] == IVault::closeCall::SELECTOR {
                return Some(Err("NotYet()".to_string()));
            }
            None
        }));
        locked.on_send = Some(Arc::new(move |node, to, data| {
            if to == VAULT
                && IVault::keepCall::abi_decode(data).is_ok_and(|keep| !keep.total.is_zero())
            {
                *node.vars.get_mut("refused").unwrap() += 1;
                return Some(json!({"code": 3, "message": "execution reverted: NotBlessed()"}));
            }
            if to == CLOSER {
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
                "signature": "0x01",
            }))
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;

    let dir = std::env::temp_dir().join(format!("keeper-closer-only-hold-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("chains.toml");
    std::fs::write(
        &config,
        format!(
            "[chains.\"evm:31337\"]\nlabel = \"stand-in\"\nrpc = \"{rpc}\"\ndollar = \"USDC\"\n\n[networks.local]\nbase = \"evm:31337\"\n"
        ),
    )
    .unwrap();
    let mut with_closer = deployments(31337);
    with_closer.closer = CLOSER;
    let mut child = Command::new(env!("CARGO_BIN_EXE_keeper"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(
            "SHONIN_DEPLOYMENTS_JSON",
            serde_json::to_string(&vec![with_closer]).unwrap(),
        )
        .args(["--network", "local", "--vault-chain", "base"])
        .arg("--chains")
        .arg(&config)
        .arg("--deployments-dir")
        .arg(&dir)
        .arg("--store")
        .arg(&dir)
        .args([
            "--api-url",
            "http://127.0.0.1:1",
            "--executor-url",
            &executor,
        ])
        // The keeper's own clock is let off; the chain's is waited on.
        .args(["--own-clock-hold", "0"])
        .stdout(std::fs::File::create(dir.join("keeper.log")).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Fifteen minutes of chain time is fifteen rounds; sixty is an hour.
    let closed = wait_until(Duration::from_secs(240), || {
        node.lock().unwrap().vars["closed"] == 1 || rounds.load(Ordering::SeqCst) >= 60
    })
    .await
        && node.lock().unwrap().vars["closed"] == 1;
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    let refused = node.lock().unwrap().vars["refused"];
    assert!(refused > 0, "setup: the keeper never tried the bill");
    assert!(
        closed,
        "after {} rounds (a minute of chain time each, the keeper's own hold let off) the \
         Closer was never sent: the bill was refused {refused} times, and its hold's fifteen \
         minutes never ran out",
        rounds.load(Ordering::SeqCst)
    );
}
