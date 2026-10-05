//! The chain's deployment names a Closer that has nothing deployed at its
//! address (a deployment file left from an earlier chain, say). A call to an
//! address with no code returns nothing and "goes through", so a simulation
//! of `closeHalted` passes every time, and a send to it is mined and does
//! nothing.
//!
//! A halted vault past its grace whose stopped station still holds a $100
//! remnant that no unwind moves can't close yet. The keeper reads whether
//! anything is deployed at the Closer's address before it uses it, finds
//! nothing, says so, and closes halted vaults step by step: the empty
//! Closer is never paid for.

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
    STATION, VAULT, WAD, a_spec, a_tank, answers, deployments, name, node, one_vault, sent,
    serve_node, timers, wait_until,
};

const CLOSER: Address = Address::repeat_byte(0xc1);

#[tokio::test(flavor = "multi_thread")]
async fn a_closer_with_nothing_deployed_is_never_paid_for() {
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
            mark: U256::from(100_000_000u64),
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
        // No code at the Closer's address: a call to it returns nothing.
        locked.empty.push(CLOSER);
        locked.vars.insert("rounds", 0);
        locked.on_call = Some(Arc::new(move |node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                node.timestamp += 10;
                *node.vars.get_mut("rounds").unwrap() += 1;
                return Some(Ok(IVault::stateCall::abi_encode_returns(
                    &VaultState::Killed,
                )));
            }
            // The probe: the remnant is where it was after the unwind.
            if to == Address::ZERO {
                let mut returned = vec![0u8; 64];
                returned.extend_from_slice(&U256::from(100_000_000u64).to_be_bytes::<32>());
                return Some(Ok(returned));
            }
            None
        }));
    }
    let rpc = serve_node(node.clone()).await;

    // The keeper, as `process::start_keeper` starts it, with a Closer named.
    let dir = std::env::temp_dir().join(format!("keeper-empty-closer-{}", std::process::id()));
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
            "http://127.0.0.1:1",
        ])
        .stdout(std::fs::File::create(dir.join("keeper.log")).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let ran = wait_until(Duration::from_secs(60), || {
        node.lock().unwrap().vars["rounds"] >= 11
    })
    .await;
    let _ = child.kill();
    let _ = child.wait();
    let logged = std::fs::read_to_string(dir.join("keeper.log")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ran, "the keeper did not run eleven rounds");

    let closers = sent::<ICloser::closeHaltedCall>(&node, CLOSER);
    assert_eq!(
        closers, 0,
        "{closers} paid calls to a Closer with nothing deployed, in ten slots, none of which \
         closed anything"
    );
    let warned = logged
        .lines()
        .filter(|line| line.contains("WARN") && line.contains("nothing is deployed"))
        .count();
    assert_eq!(
        warned, 1,
        "the empty Closer was said {warned} times: {logged}"
    );
}
