//! A failed read skips only the steps that need it, and is said at warn: at
//! once, then at most every ten minutes with a count, for each chain, vault
//! and step.
//!
//! 1. Two chains each hold a vault at the same address (vaults are clones at
//!    an address fixed by the strategy id, and the devnet deploys every chain
//!    from the same account in the same order). Both fail the same read: each
//!    chain's failure is said, and names its chain.
//! 2. One vault's read fails, works again, and fails again a few rounds
//!    later: the new failure is said at once.

mod process;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use chain::bindings::station::IStationVault;
use chain::bindings::vault::{IVault, VaultState};
use process::{
    Shared, VAULT, a_tank, answers, node, one_vault, serve, serve_node, start_keeper,
    start_keeper_on, timers, wait_until,
};

const STEP: &str = "read the vault's home station";

/// An open vault whose home station can't be read (unscripted: the read
/// reverts), everything else readable.
fn an_open_vault_without_a_station(node: &Shared) {
    one_vault(node);
    answers(node, VAULT, IVault::stateCall {}, VaultState::Open);
    answers(node, VAULT, IVault::timersCall {}, timers());
    answers(node, VAULT, IVault::tankCall {}, a_tank(50_000_000));
    answers(node, VAULT, IStationVault::haltedAtCall {}, 0u64);
    answers(node, VAULT, IStationVault::unsettledCall {}, false);
    answers(node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(node, VAULT, IVault::claimCountCall {}, U256::ZERO);
}

fn warned(logged: &str) -> Vec<String> {
    logged
        .lines()
        .filter(|line| line.contains("WARN") && line.contains(STEP))
        .map(str::to_string)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_step_failing_on_two_chains_is_said_for_each_chain() {
    let start = 1_700_000_005u64;
    let base = node(start);
    let hyper = node(start);
    hyper.lock().unwrap().chain_id = 31338;
    an_open_vault_without_a_station(&base);
    an_open_vault_without_a_station(&hyper);
    let executor = serve(axum::Router::new()).await; // 404 for everything
    let base_rpc = serve_node(base.clone()).await;
    let hyper_rpc = serve_node(hyper.clone()).await;
    let keeper = start_keeper_on(
        "step-two-chains",
        &[
            ("base", 31337, base_rpc.as_str()),
            ("hyperliquid", 31338, hyper_rpc.as_str()),
        ],
        &executor,
    );

    // Both chains' rounds have run, and failed the read, several times.
    assert!(
        wait_until(Duration::from_secs(60), || {
            let count = |node: &Shared| {
                node.lock()
                    .unwrap()
                    .calls
                    .iter()
                    .filter(|(to, sel)| *to == VAULT && *sel == IVault::homeStationCall::SELECTOR)
                    .count()
            };
            count(&base) >= 3 && count(&hyper) >= 3
        })
        .await,
        "setup: both chains' rounds did not run"
    );
    tokio::time::sleep(Duration::from_secs(1)).await;
    let lines = warned(&keeper.logged());
    for chain_id in ["31337", "31338"] {
        assert!(
            lines.iter().any(|line| line.contains(chain_id)),
            "the same step failed for the vault on two chains, but no warning names {chain_id}, \
             so that chain's failure went unheard for up to ten minutes: {lines:#?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_step_that_fails_again_after_working_is_said_at_once() {
    let start = 1_700_000_005u64;
    let node = node(start);
    an_open_vault_without_a_station(&node);
    {
        let mut locked = node.lock().unwrap();
        locked.vars.insert("rounds", 0);
        locked.on_call = Some(Arc::new(|node, to, data| {
            if to == VAULT && data[..4] == IVault::stateCall::SELECTOR {
                *node.vars.get_mut("rounds").unwrap() += 1;
            }
            // Rounds 4 to 6 read the station; the rest can't.
            if to == VAULT && data[..4] == IVault::homeStationCall::SELECTOR {
                let round = node.vars["rounds"];
                if (4..=6).contains(&round) {
                    return Some(Ok(IVault::homeStationCall::abi_encode_returns(
                        &process::STATION,
                    )));
                }
            }
            None
        }));
    }
    let executor = serve(axum::Router::new()).await;
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("step-fails-again", &rpc, &executor);

    assert!(
        wait_until(
            Duration::from_secs(60),
            || node.lock().unwrap().vars["rounds"] >= 10
        )
        .await,
        "setup: the keeper did not run"
    );
    let lines = warned(&keeper.logged());
    assert!(
        lines.len() >= 2,
        "the read failed (rounds 1-3), worked (4-6) and failed again (7 on), but the new \
         failure was not said at once: {lines:#?}"
    );
}
