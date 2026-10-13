//! A read that a batched call could not finish (the batch ran out of gas
//! inside it) is never reported as reverted: a revert is an answer the
//! consumer may act on, while the same read alone at the same block returns
//! a value. The helper path tells the two apart (`OutOfGas` is read again);
//! on the Multicall3 path, which reports running out of gas as a revert with
//! no data, such a read is read alone at the block before it is an answer,
//! and a read the reads before it starved makes later calls take fewer.

mod stand_in_node;

use alloy::primitives::{Bytes, U256};
use alloy_sol_types::SolCall;
use chain::follow::{At, Item, Path, Plan};
use serde_json::json;
use stand_in_node::{Anvil, FollowProbe, PROBE, Proxy, policy};

/// A chain with the probe (value 5) and the stand-in Multicall3, behind a
/// proxy that refuses overrides, so Multicall3 is the path.
async fn chain(id: u64) -> (Anvil, Proxy) {
    let anvil = Anvil::start(id, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    stand_in_node::put_probe(&anvil, PROBE, 5).await;
    anvil
        .set_code(
            stand_in_node::MULTICALL,
            &stand_in_node::code("StandInMulticall3"),
        )
        .await;
    anvil.mine(3, 1).await;
    let proxy = Proxy::start(&anvil.url).await;
    proxy.script(|script| script.refuse_overrides = true);
    (anvil, proxy)
}

/// `read` alone, on the node itself, at `number`.
async fn alone(anvil: &Anvil, read: &impl SolCall, number: u64) -> Bytes {
    serde_json::from_value(
        anvil
            .rpc(
                "eth_call",
                json!([{"to": PROBE, "data": Bytes::from(read.abi_encode())}, format!("0x{number:x}")]),
            )
            .await
            .expect("the read returns when read alone"),
    )
    .unwrap()
}

#[tokio::test]
async fn a_read_a_multicall3_batch_could_not_run_is_never_reported_as_reverted() {
    let (anvil, proxy) = chain(31_480_001).await;
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    let mut plan = Plan::default();
    let heavy = FollowProbe::burnCall {
        rounds: U256::from(200_000),
    };
    plan.read(PROBE, &heavy);
    plan.read(PROBE, &FollowProbe::valueCall {});
    // The endpoint caps a call's gas without saying so: below what the
    // heavy read needs inside the batch (about 22M), far above what the
    // cheap read needs. A read alone is not capped here, as an endpoint
    // whose cap is above the read's need would not.
    for cap in [10_000_000u64, 15_000_000, 20_000_000] {
        proxy.script(|script| script.gas_cap = Some(cap));
        // No observation at all would be acceptable; a wrong one is not.
        let observed = match follower.observe(&plan, At::Latest).await {
            Ok(observed) => observed,
            Err(error) => {
                eprintln!("gas cap {cap}: no observation: {error:#}");
                continue;
            }
        };
        assert_eq!(observed.path, Path::Multicall3);
        // The heavy read alone, at the block the observation ran at, returns.
        let alone = alone(&anvil, &heavy, observed.number).await;
        assert_eq!(
            U256::from_be_slice(&alone),
            U256::from(5 + 200_000),
            "the heavy read alone"
        );
        assert_eq!(
            observed.items[1],
            Item::Returned(U256::from(5).to_be_bytes::<32>().to_vec().into()),
            "the cheap read"
        );
        // The observation must say what the read alone says, or leave it
        // out (an error): never that it reverted.
        assert_eq!(
            observed.items[0],
            Item::Returned(alone),
            "gas cap {cap}: a read the batch ran out of gas in, reported as an answer"
        );
        // The gas the endpoint gives a call is found out and reported.
        let gas = observed.gas_at_entry.expect("the call's gas is known");
        assert!(gas <= cap, "gas {gas} under a cap of {cap}");
    }
}

#[tokio::test]
async fn a_read_the_reads_before_it_starved_is_read_alone_and_later_batches_take_fewer() {
    let (anvil, proxy) = chain(31_480_004).await;
    // The two costly reads need about 2.2M and 7.7M gas: under a 10M cap
    // the first in a batch returns, the second finds too little left, and
    // what it leaves is enough for the cheap read after it and the answer.
    proxy.script(|script| script.gas_cap = Some(10_000_000));
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    let mut plan = Plan::default();
    let costly: Vec<_> = [20_000u64, 70_000]
        .into_iter()
        .map(|rounds| FollowProbe::burnCall {
            rounds: U256::from(rounds),
        })
        .collect();
    for read in &costly {
        plan.read(PROBE, read);
    }
    plan.read(PROBE, &FollowProbe::valueCall {});
    let five = Item::Returned(U256::from(5).to_be_bytes::<32>().to_vec().into());
    let first = follower.observe(&plan, At::Latest).await.unwrap();
    assert_eq!(first.path, Path::Multicall3);
    for (index, read) in costly.iter().enumerate() {
        assert_eq!(
            first.items[index],
            Item::Returned(alone(&anvil, read, first.number).await),
            "read {index}"
        );
    }
    assert_eq!(first.items[2], five);
    // The header, the call, and the starved read alone.
    assert_eq!(first.requests, 3);
    // Starved second in its call: calls here now take one read.
    assert_eq!(follower.capabilities(0).unwrap().max_reads, Some(1));
    // The next observation splits the plan and needs no read alone: the
    // header and three calls.
    anvil.mine(1, 1).await;
    let next = follower.observe(&plan, At::Latest).await.unwrap();
    assert_eq!(next.requests, 4);
    for (index, read) in costly.iter().enumerate() {
        assert_eq!(
            next.items[index],
            Item::Returned(alone(&anvil, read, next.number).await),
            "read {index}"
        );
    }
    assert_eq!(next.items[2], five);
}
