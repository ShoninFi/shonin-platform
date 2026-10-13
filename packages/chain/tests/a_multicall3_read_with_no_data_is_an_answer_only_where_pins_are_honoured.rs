//! On the Multicall3 path a read that fails with no data is read alone,
//! pinned to the block's hash, before it is an answer. A read that truly
//! reverts with no data is then reported as that revert, with nothing
//! learned of the endpoint's gas; one the reads before it starved is
//! reported as the value it returns alone; where the endpoint has not been
//! shown to honour hash pins, the observation fails rather than guess. A
//! plan many times over the cap is still read whole, each read equal to
//! itself alone, with the learned split never reaching zero.

mod stand_in_node;

use alloy::primitives::{Bytes, U256};
use alloy_sol_types::SolCall;
use chain::follow::{At, Item, Path, Plan};
use serde_json::json;
use stand_in_node::{Anvil, FollowProbe, PROBE, Proxy, policy};

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

/// `read` alone, on the node itself, at `number`: what it returned, or
/// that it reverted.
async fn alone(anvil: &Anvil, read: &impl SolCall, number: u64) -> Item {
    match anvil
        .rpc(
            "eth_call",
            json!([{"to": PROBE, "data": Bytes::from(read.abi_encode())}, format!("0x{number:x}")]),
        )
        .await
    {
        Ok(value) => Item::Returned(serde_json::from_value(value).unwrap()),
        Err(error) => {
            assert!(error.contains("revert"), "{error}");
            Item::Reverted(Bytes::new())
        }
    }
}

#[tokio::test]
async fn a_read_that_reverts_with_no_data_is_reported_so_and_teaches_nothing() {
    let (_anvil, proxy) = chain(31_490_002).await;
    proxy.script(|script| script.gas_cap = Some(10_000_000));
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    let mut plan = Plan::default();
    plan.read(PROBE, &FollowProbe::valueCall {});
    plan.read(PROBE, &FollowProbe::refuseEmptyCall {});
    plan.read(PROBE, &FollowProbe::valueCall {});
    let five = Item::Returned(U256::from(5).to_be_bytes::<32>().to_vec().into());
    let observed = follower.observe(&plan, At::Latest).await.unwrap();
    assert_eq!(observed.path, Path::Multicall3);
    assert_eq!(observed.items[0], five);
    assert_eq!(observed.items[1], Item::Reverted(Bytes::new()));
    assert_eq!(observed.items[2], five);
    // A revert with no data says nothing of the endpoint's gas.
    assert_eq!(follower.capabilities(0).unwrap().max_reads, None);
    // And the next observation does not probe the endpoint again: the
    // header, the call and the one read alone.
    proxy.forget_seen();
    let next = follower.observe(&plan, At::Latest).await.unwrap();
    assert_eq!(next.items, observed.items);
    assert_eq!(next.requests, 3, "{:?}", proxy.seen().methods);
    assert_eq!(
        proxy.seen().count("eth_call"),
        2,
        "{:?}",
        proxy.seen().methods
    );
}

#[tokio::test]
async fn where_pins_are_not_honoured_a_read_with_no_data_fails_the_observation() {
    let (_anvil, proxy) = chain(31_490_003).await;
    proxy.script(|script| script.ignore_pins = true);
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    let mut plan = Plan::default();
    plan.read(PROBE, &FollowProbe::valueCall {});
    plan.read(PROBE, &FollowProbe::refuseEmptyCall {});
    let result = follower.observe(&plan, At::Latest).await;
    // No observation is right here; a reported revert is a guess.
    assert!(result.is_err(), "{result:?}");
    assert_eq!(follower.capabilities(0).unwrap().hash_pin, Some(false));
}

#[tokio::test]
async fn a_plan_many_times_over_the_cap_is_read_whole_and_the_split_never_reaches_zero() {
    let (anvil, proxy) = chain(31_490_004).await;
    // Sixteen reads of about 2.2M gas each under a 10M cap: at most four fit
    // one call, and the first call also carries the window's hashes.
    proxy.script(|script| script.gas_cap = Some(10_000_000));
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    let mut plan = Plan::default();
    let reads: Vec<_> = (0..16u64)
        .map(|i| FollowProbe::burnCall {
            rounds: U256::from(20_000 + i),
        })
        .collect();
    for read in &reads {
        plan.read(PROBE, read);
    }
    plan.read(PROBE, &FollowProbe::refuseEmptyCall {});
    for turn in 0..3 {
        let observed = follower.observe(&plan, At::Latest).await.unwrap();
        assert_eq!(observed.path, Path::Multicall3, "turn {turn}");
        for (index, read) in reads.iter().enumerate() {
            assert_eq!(
                observed.items[index],
                alone(&anvil, read, observed.number).await,
                "turn {turn}, read {index}"
            );
        }
        assert_eq!(observed.items[16], Item::Reverted(Bytes::new()));
        let learned = follower.capabilities(0).unwrap().max_reads;
        assert!(
            learned.is_some_and(|reads| (1..=4).contains(&reads)),
            "turn {turn}: max_reads {learned:?}"
        );
        eprintln!(
            "turn {turn}: {} requests, max_reads {learned:?}",
            observed.requests
        );
        anvil.mine(1, 1).await;
    }
}
