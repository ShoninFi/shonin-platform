//! A halted vault past its grace, a Closer deployed and its `closeHalted`
//! simulating as going through in every slot's window. The node turns every
//! send of it away for a reason of its own ("insufficient funds for gas *
//! price + value": the Closer goes out with a gas limit of one and a half
//! times its estimate and 250,000 more, the biggest the keeper sends, so it
//! is the first a thin balance can't cover). Every other send goes out.
//!
//! A send that doesn't go out is said at warn and shown in the keeper's
//! health until it goes through, the Closer's as much as any other: the
//! one-transaction close is the one nobody can get between.

mod process;

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::closer::ICloser;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    STATION, VAULT, WAD, a_spec, a_tank, answers, deployments, name, node, one_vault, serve_node,
    timers, wait_until,
};
use serde_json::json;

const CLOSER: Address = Address::repeat_byte(0xc1);

#[tokio::test(flavor = "multi_thread")]
async fn a_closer_send_that_does_not_go_out_is_heard_of() {
    // Each round at a slot's start: the next block lands inside the window.
    let node = node(1_700_000_000);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::tankCall {}, a_tank(0));
    answers(&node, VAULT, IStationVault::haltedAtCall {}, 1_699_000_000);
    answers(&node, VAULT, IStationVault::haltGraceCall {}, 600);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
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
    // The close would go through.
    answers(
        &node,
        CLOSER,
        ICloser::closeHaltedCall { vault: VAULT },
        ICloser::closeHaltedReturn {},
    );
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 10;
                *node.vars.get_mut("rounds").unwrap() += 1;
                return Some(Ok(IVault::stateCall::abi_encode_returns(
                    &VaultState::Killed,
                )));
            }
            None
        }));
        locked.on_send = Some(Arc::new(|node, to, _| {
            (to == CLOSER).then(|| {
                *node.vars.entry("closers_turned_away").or_insert(0) += 1;
                json!({
                    "code": -32000,
                    "message": "insufficient funds for gas * price + value: balance 1000, tx cost 2000"
                })
            })
        }));
    }
    let rpc = serve_node(node.clone()).await;

    let dir = std::env::temp_dir().join(format!("keeper-closer-not-sent-{}", std::process::id()));
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
            "http://127.0.0.1:1",
        ])
        .args(["--port", &port.to_string()])
        .stdout(std::fs::File::create(dir.join("keeper.log")).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let ran = wait_until(Duration::from_secs(60), || {
        node.lock().unwrap().vars["rounds"] >= 6
    })
    .await;
    let health = async {
        let reply = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/health"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .ok()?;
        let status = reply.status().as_u16();
        Some((status, reply.text().await.unwrap_or_default()))
    }
    .await;
    let _ = child.kill();
    let _ = child.wait();
    let logged = std::fs::read_to_string(dir.join("keeper.log")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ran, "the keeper did not run six rounds");
    let turned_away = node
        .lock()
        .unwrap()
        .vars
        .get("closers_turned_away")
        .copied()
        .unwrap_or(0);
    assert!(
        turned_away >= 3,
        "the Closer was only tried {turned_away} times"
    );
    let warned = logged
        .lines()
        .any(|line| line.contains("WARN") && line.contains("did not go out"));
    assert!(
        health
            .as_ref()
            .is_some_and(|(status, said)| *status == 503 && said.contains("not going out")),
        "the node turned the Closer away {turned_away} times and the keeper's health says \
         {health:?} (warned: {warned})"
    );
}
