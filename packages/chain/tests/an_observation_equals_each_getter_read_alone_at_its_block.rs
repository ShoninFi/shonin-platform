//! An observation is the same as every one of its reads made alone at the
//! block it ran at: each getter's return data or revert data, each
//! balance, each block hash. This holds on every path (the helper, the
//! Multicall3 fallback and single pinned reads), for a plan in one call and
//! for one split over many, and for an observation pinned to a header.

mod stand_in_node;

use alloy::primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use chain::follow::{At, Follower, Header, Item, Observed, Path, Plan};
use serde_json::json;
use stand_in_node::{Anvil, FollowProbe, PROBE, PROBE_TWO, Proxy, policy};

/// Every getter of the probe, with arguments that make it return, revert,
/// overflow the result cap (once within the helper's largest cap, once
/// beyond it) and run out of the helper's per-read gas; and reads of an
/// account with no code and of a function the probe does not have.
fn plan() -> Plan {
    let mut plan = Plan::default();
    let holder = Address::repeat_byte(0x55);
    for target in [PROBE, PROBE_TWO] {
        plan.read(target, &FollowProbe::valueCall {});
        plan.read(target, &FollowProbe::creditCall(holder));
        plan.read(target, &FollowProbe::creditCall(Address::ZERO));
        plan.read(target, &FollowProbe::pairCall {});
        plan.read(target, &FollowProbe::tripleCall {});
        plan.read(target, &FollowProbe::nameCall {});
        for n in [0u64, 3, 40] {
            plan.read(target, &FollowProbe::listCall { n: U256::from(n) });
        }
        for n in [0u64, 31, 33, 2_000, 20_000] {
            plan.read(target, &FollowProbe::blobCall { n: U256::from(n) });
        }
        plan.read(target, &FollowProbe::refuseCall {});
        plan.read(target, &FollowProbe::refuseWithReasonCall {});
        plan.read(target, &FollowProbe::refuseEmptyCall {});
        for rounds in [0u64, 10, 100_000] {
            plan.read(
                target,
                &FollowProbe::burnCall {
                    rounds: U256::from(rounds),
                },
            );
        }
        plan.read(target, &FollowProbe::contextCall {});
    }
    plan.reads.push(chain::follow::Read {
        target: Address::repeat_byte(0x77),
        data: Bytes::from_static(&[1, 2, 3, 4]),
    });
    plan.reads.push(chain::follow::Read {
        target: PROBE,
        data: Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
    });
    for of in [
        Address::repeat_byte(0x55),
        Address::repeat_byte(0x66),
        PROBE,
    ] {
        plan.balance(of);
    }
    plan.hashes = vec![3, 60, 100];
    plan
}

/// Each read alone, pinned to the observed block's number on the node
/// itself: the observation must say the same.
async fn the_same_as_alone(anvil: &Anvil, plan: &Plan, observed: &Observed) {
    let block = format!("0x{:x}", observed.number);
    assert_eq!(observed.items.len(), plan.reads.len());
    for (index, (read, item)) in plan.reads.iter().zip(&observed.items).enumerate() {
        let alone = anvil
            .rpc(
                "eth_call",
                json!([{"from": Address::ZERO, "to": read.target, "data": read.data}, block]),
            )
            .await;
        match (alone, item) {
            (Ok(data), Item::Returned(returned)) => {
                let data: Bytes = serde_json::from_value(data).unwrap();
                assert_eq!(&data, returned, "read {index} ({:?})", observed.path);
            }
            (Err(error), Item::Reverted(reverted)) => {
                let error: serde_json::Value = serde_json::from_str(&error).unwrap();
                let data: Bytes = error
                    .get("data")
                    .and_then(|data| serde_json::from_value(data.clone()).ok())
                    .unwrap_or_default();
                assert_eq!(&data, reverted, "read {index} ({:?})", observed.path);
            }
            (alone, item) => panic!(
                "read {index} ({:?}): alone {alone:?}, observed {item:?}",
                observed.path
            ),
        }
    }
    for (of, balance) in plan.balances.iter().zip(&observed.balances) {
        let alone = anvil
            .rpc("eth_getBalance", json!([of, block]))
            .await
            .unwrap();
        let alone: U256 = serde_json::from_value(alone).unwrap();
        assert_eq!(&alone, balance);
    }
    let (_, parent, timestamp) = anvil.block(observed.number).await;
    assert_eq!(observed.parent_hash, parent);
    assert_eq!(observed.timestamp, timestamp);
    for (number, hash) in &observed.hashes {
        assert_eq!(anvil.block(*number).await.0, *hash, "block {number}'s hash");
    }
    // The helper and Multicall3 carry the window below the block.
    if observed.path != Path::Individual {
        for number in observed.number.saturating_sub(256).max(1)..observed.number {
            assert!(observed.hashes.contains_key(&number), "block {number}");
        }
        // An asked-for block inside the window is carried; one outside it
        // has no hash to give.
        for number in &plan.hashes {
            let inside = observed.number - number <= 256;
            assert_eq!(
                observed.hashes.contains_key(number),
                inside,
                "asked-for block {number}"
            );
        }
    }
    assert!(observed.zero.is_empty());
}

async fn chain(id: u64) -> Anvil {
    let anvil = Anvil::start(id, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    stand_in_node::put_probe(&anvil, PROBE, 42).await;
    stand_in_node::put_probe(&anvil, PROBE_TWO, 7).await;
    anvil
        .send(
            PROBE,
            FollowProbe::giveCall {
                to: Address::repeat_byte(0x55),
                amount: U256::from(5),
            }
            .abi_encode(),
        )
        .await;
    anvil
        .fund(
            Address::repeat_byte(0x55),
            U256::from(10u64).pow(U256::from(18)),
        )
        .await;
    anvil.mine(3, 1).await;
    anvil
}

async fn header(anvil: &Anvil, number: u64) -> Header {
    let (hash, parent_hash, timestamp) = anvil.block(number).await;
    Header {
        number,
        hash,
        parent_hash,
        timestamp,
    }
}

async fn every_way(anvil: &Anvil, follower: &mut Follower, path: Path) {
    let plan = plan();
    let observed = follower.observe(&plan, At::Latest).await.unwrap();
    assert_eq!(observed.path, path);
    assert_eq!(observed.number, anvil.head().await);
    the_same_as_alone(anvil, &plan, &observed).await;
    // Pinned to an older header.
    let pinned = header(anvil, observed.number - 2).await;
    let observed = follower.observe(&plan, At::Pinned(pinned)).await.unwrap();
    assert_eq!(observed.path, path);
    assert_eq!(
        (observed.number, observed.hash),
        (pinned.number, Some(pinned.hash))
    );
    the_same_as_alone(anvil, &plan, &observed).await;
}

#[tokio::test]
async fn the_helper_s_observation_equals_each_read_alone() {
    let anvil = chain(31_410_001).await;
    let mut follower = stand_in_node::follower(&[&anvil.url], policy()).await;
    every_way(&anvil, &mut follower, Path::ViewReads).await;
}

#[tokio::test]
async fn an_observation_split_over_many_calls_equals_each_read_alone() {
    let anvil = chain(31_410_002).await;
    // Three reads a call: the plan takes a dozen calls and more follow-ups.
    let mut policy = policy();
    policy.view_batch_reads = 3;
    let mut follower = stand_in_node::follower(&[&anvil.url], policy).await;
    every_way(&anvil, &mut follower, Path::ViewReads).await;
    let observed = follower.observe(&plan(), At::Latest).await.unwrap();
    assert!(observed.requests >= 15, "{} requests", observed.requests);
}

#[tokio::test]
async fn the_multicall3_observation_equals_each_read_alone() {
    let anvil = chain(31_410_003).await;
    anvil
        .set_code(
            stand_in_node::MULTICALL,
            &stand_in_node::code("StandInMulticall3"),
        )
        .await;
    // Code put in place is in the blocks mined after it, and the pinned
    // observation reads two blocks back.
    anvil.mine(3, 1).await;
    let proxy = Proxy::start(&anvil.url).await;
    proxy.script(|script| script.refuse_overrides = true);
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    every_way(&anvil, &mut follower, Path::Multicall3).await;
    let capabilities = follower.capabilities(0).unwrap();
    assert_eq!(capabilities.overrides, Some(false));
    assert_eq!(capabilities.multicall3, Some(true));
}

#[tokio::test]
async fn single_pinned_reads_equal_each_read_alone() {
    let anvil = chain(31_410_004).await;
    let proxy = Proxy::start(&anvil.url).await;
    proxy.script(|script| script.refuse_overrides = true);
    // No Multicall3 where the follower looks for it.
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    every_way(&anvil, &mut follower, Path::Individual).await;
    let capabilities = follower.capabilities(0).unwrap();
    assert_eq!(capabilities.multicall3, Some(false));
    assert_eq!(capabilities.hash_pin, Some(true));
}

#[tokio::test]
async fn reads_the_helper_leaves_open_are_read_again_at_the_same_block() {
    // A plan of only the reads the helper cannot finish in one go: over
    // the result cap and out of per-read gas.
    let anvil = chain(31_410_005).await;
    let mut follower = stand_in_node::follower(&[&anvil.url], policy()).await;
    let mut plan = Plan::default();
    plan.read(
        PROBE,
        &FollowProbe::blobCall {
            n: U256::from(5_000),
        },
    );
    plan.read(
        PROBE,
        &FollowProbe::blobCall {
            n: U256::from(50_000),
        },
    );
    plan.read(
        PROBE,
        &FollowProbe::burnCall {
            rounds: U256::from(100_000),
        },
    );
    plan.read(PROBE, &FollowProbe::valueCall {});
    let observed = follower.observe(&plan, At::Latest).await.unwrap();
    // The first call, the header to pin the follow-ups to, the larger
    // re-read, and two single reads.
    assert_eq!(observed.requests, 5);
    assert_eq!(observed.hash, Some(anvil.block(observed.number).await.0));
    the_same_as_alone(&anvil, &plan, &observed).await;
    assert_eq!(
        observed.decode::<FollowProbe::valueCall>(3).unwrap(),
        U256::from(42)
    );
}
