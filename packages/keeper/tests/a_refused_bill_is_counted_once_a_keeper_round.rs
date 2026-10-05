//! A vault's refused bill is counted once a keeper round, and shown in the
//! keeper's health as "N rounds in a row", however often the round tries
//! it and whatever the chain's clock does meanwhile.
//!
//! A halted vault past its grace that only the Closer can close (as in
//! `a_close_only_the_closer_can_make_is_not_held_for_good`), its bill
//! refused for good. Its round tries the bill at the top and again on the
//! Closer's path, which reads the chain's clock afresh; the chain's clock
//! moves a second during each round, as any real chain's does when a block
//! lands mid-round. The health line never says more rounds in a row than
//! the keeper has run.

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
async fn a_refused_bill_is_counted_once_a_keeper_round() {
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
        locked.vars.insert("moved", 0);
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                // Each round at a slot's start, a minute after the last.
                let round = counted.fetch_add(1, Ordering::SeqCst) + 1;
                node.timestamp = start + 60 * round;
                node.vars.insert("moved", 0);
                return Some(Ok(IVault::stateCall::abi_encode_returns(
                    &VaultState::Killed,
                )));
            }
            // A block lands while the round goes on: a second later, still
            // inside the slot's window.
            if to == VAULT
                && data[..4] == IVault::homeStationCall::SELECTOR
                && node.vars["moved"] == 0
            {
                node.vars.insert("moved", 1);
                node.timestamp += 1;
            }
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

    let dir = std::env::temp_dir().join(format!(
        "keeper-refused-counted-once-{}",
        std::process::id()
    ));
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
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
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
        // The close's hold for the bill runs on the keeper's own clock as
        // well as the chain's, and is not over before the health is read:
        // once over, the refused bill is written off and no longer said.
        .args(["--port", &port.to_string()])
        .stdout(std::fs::File::create(dir.join("keeper.log")).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let health = async {
        let reply = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/health"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .ok()?;
        Some(reply.text().await.unwrap_or_default())
    };
    assert!(
        wait_until(Duration::from_secs(120), || rounds.load(Ordering::SeqCst)
            >= 8)
        .await,
        "setup: the keeper did not run"
    );
    let said = health.await.unwrap_or_default();
    // Rounds only go up: read after the health line, they bound it.
    let ran = rounds.load(Ordering::SeqCst);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    let counted: Option<u64> = said
        .lines()
        .find(|line| line.starts_with("bills the vault refuses"))
        .and_then(|line| line.split('(').nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok());
    let counted = counted.unwrap_or_else(|| panic!("setup: no refused bill in health: {said}"));
    assert!(
        counted <= ran,
        "the health line says the bill was refused {counted} rounds in a row after only {ran} \
         rounds: a round counted its refusal more than once; health: {said}"
    );
}
