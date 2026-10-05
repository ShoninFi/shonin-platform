//! Six open vaults, each with a tank and a bill to collect, and an executor
//! that takes the connection and never answers (a hung VM, a firewall that
//! drops). Its asks run off the round, so the rounds go on at their own pace
//! (a round is two seconds when nothing waits on the executor): every vault
//! is still valued, settled and tried for claims in its slot.

mod process;

use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, U256};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    FACTORY, IReplacesFactory, STATION, a_spec, a_tank, answers, called, node, serve, serve_node,
    start_keeper, timers, wait_until,
};

#[tokio::test(flavor = "multi_thread")]
async fn an_executor_that_hangs_does_not_hold_the_rounds_up() {
    let started = Instant::now();
    let node = node(1_700_000_005);
    let vaults: Vec<Address> = (0x70u8..0x76).map(Address::repeat_byte).collect();
    answers(&node, FACTORY, IVaultFactory::vaultsCall {}, vaults.clone());
    answers(
        &node,
        FACTORY,
        IReplacesFactory::previousCall {},
        Address::ZERO,
    );
    for &vault in &vaults {
        answers(&node, vault, IVault::stateCall {}, VaultState::Open);
        answers(&node, vault, IVault::timersCall {}, timers());
        answers(&node, vault, IVault::tankCall {}, a_tank(5_000_000));
        answers(&node, vault, IStationVault::haltedAtCall {}, 0);
        answers(&node, vault, IVault::homeStationCall {}, STATION);
        answers(&node, vault, IVault::valuationCountCall {}, U256::ZERO);
        answers(&node, vault, IStationVault::unsettledCall {}, false);
        answers(&node, vault, IVault::claimCountCall {}, U256::ZERO);
    }
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![]));
    answers(&node, STATION, IStation::outsideBandCall {}, false);
    // Chain time runs ten times the wall clock: a chain-minute is six
    // seconds, and every vault's bill is asked for again each one.
    node.lock().unwrap().on_call = Some(Arc::new(move |node, _, _| {
        node.timestamp = 1_700_000_005 + 10 * started.elapsed().as_secs();
        None
    }));
    let hanging = serve(axum::Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(|| async {
            tokio::time::sleep(Duration::from_secs(3_600)).await;
            "never"
        }),
    ))
    .await;
    let rpc = serve_node(node.clone()).await;
    let _keeper = start_keeper("hanging-executor", &rpc, &hanging);
    let first = vaults[0];

    assert!(
        wait_until(Duration::from_secs(60), || {
            called::<IVault::stateCall>(&node, first) >= 2
        })
        .await,
        "the second round never started"
    );
    let second_started = started.elapsed();
    let rounds_before = called::<IVault::stateCall>(&node, first);
    tokio::time::sleep(Duration::from_secs(20)).await;
    let rounds = called::<IVault::stateCall>(&node, first) - rounds_before;
    assert!(
        rounds >= 6,
        "the second round started {second_started:?} in; in the 20 seconds after it the keeper \
         began {rounds} more rounds while the executor hung"
    );
}
