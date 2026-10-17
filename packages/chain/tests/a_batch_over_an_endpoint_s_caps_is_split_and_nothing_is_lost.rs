//! A plan larger than one call allows is split, and nothing is lost when an
//! endpoint caps a call's gas without saying so or refuses an answer as too
//! big: reads the helper could not reach are read again at the same block,
//! a Multicall3 call that runs out of gas as a whole is asked again in
//! smaller calls, the endpoint's limit is learned, and the next observation
//! asks for less at once.

mod stand_in_node;

use alloy::primitives::U256;
use chain::follow::{At, Item, Path, Plan};
use stand_in_node::{Anvil, FollowProbe, PROBE, Proxy, policy};

fn burning(count: usize) -> Plan {
    let mut plan = Plan::default();
    for i in 0..count {
        // About 0.1M gas each.
        plan.read(
            PROBE,
            &FollowProbe::burnCall {
                rounds: U256::from(1_000 + i),
            },
        );
    }
    plan
}

async fn chain(id: u64) -> Anvil {
    let anvil = Anvil::start(id, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    stand_in_node::put_probe(&anvil, PROBE, 5).await;
    anvil.mine(2, 1).await;
    anvil
}

fn each_burned(observed: &chain::follow::Observed, count: usize) {
    assert_eq!(observed.items.len(), count);
    for (i, item) in observed.items.iter().enumerate() {
        assert_eq!(
            item,
            &Item::Returned(
                U256::from(5 + 1_000 + i)
                    .to_be_bytes::<32>()
                    .to_vec()
                    .into()
            ),
            "read {i}"
        );
    }
}

#[tokio::test]
async fn a_silent_gas_cap_splits_the_batch_and_the_split_is_learned() {
    let anvil = chain(31_440_001).await;
    let proxy = Proxy::start(&anvil.url).await;
    proxy.script(|script| script.gas_cap = Some(3_000_000));
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    let plan = burning(120);
    let first = follower.observe(&plan, At::Latest).await.unwrap();
    each_burned(&first, 120);
    let gas = first.gas_at_entry.unwrap();
    assert!(gas <= 3_000_000, "the helper saw {gas} gas");
    let learned = follower.capabilities(0).unwrap().max_reads.unwrap();
    assert!(learned < 120, "learned {learned}");
    // The next observation is split up front: no read is left for later.
    proxy.forget_seen();
    let second = follower.observe(&plan, At::Latest).await.unwrap();
    each_burned(&second, 120);
    let calls = proxy.seen().count("eth_call");
    assert!(
        calls as usize >= 120usize.div_ceil(learned),
        "{calls} calls"
    );
    // One header, to pin the calls after the first to one block.
    assert_eq!(proxy.seen().count("eth_getBlockByNumber"), 1);
}

#[tokio::test]
async fn the_configured_read_bound_splits_a_plan_into_pinned_calls() {
    let anvil = chain(31_440_002).await;
    let proxy = Proxy::start(&anvil.url).await;
    let mut policy = policy();
    policy.view_batch_reads = 16;
    let mut follower = stand_in_node::follower(&[&proxy.url], policy).await;
    // The endpoint's capabilities are asked at the first observation.
    follower
        .observe(&Plan::default(), At::Latest)
        .await
        .unwrap();
    proxy.forget_seen();
    let observed = follower.observe(&burning(100), At::Latest).await.unwrap();
    each_burned(&observed, 100);
    assert_eq!(proxy.seen().count("eth_call"), 7);
    assert_eq!(observed.hash, Some(anvil.block(observed.number).await.0));
}

#[tokio::test]
async fn an_answer_refused_as_too_big_is_asked_for_again_smaller() {
    let anvil = chain(31_440_003).await;
    let proxy = Proxy::start(&anvil.url).await;
    // About 150 kB of JSON at most per answer.
    proxy.script(|script| script.response_cap = Some(150_000));
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    let mut plan = Plan::default();
    for _ in 0..200 {
        plan.read(PROBE, &FollowProbe::blobCall { n: U256::from(900) });
    }
    let observed = follower.observe(&plan, At::Latest).await.unwrap();
    for item in &observed.items {
        let Item::Returned(data) = item else {
            panic!("{item:?}");
        };
        assert_eq!(data.len(), 32 + 32 + 928);
    }
    assert!(follower.capabilities(0).unwrap().output_bytes.unwrap() < policy().view_output_bytes);
}

#[tokio::test]
async fn a_multicall3_call_out_of_gas_as_a_whole_is_split_and_the_split_is_learned() {
    let anvil = chain(31_440_004).await;
    anvil
        .set_code(
            stand_in_node::MULTICALL,
            &stand_in_node::code("StandInMulticall3"),
        )
        .await;
    anvil.mine(2, 1).await;
    let proxy = Proxy::start(&anvil.url).await;
    // Overrides refused, so Multicall3 is the path; three reads of about
    // 5.5M gas each under a 10M cap leave the call too little to return.
    proxy.script(|script| {
        script.refuse_overrides = true;
        script.gas_cap = Some(10_000_000);
    });
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    let mut plan = Plan::default();
    for i in 0..3u64 {
        plan.read(
            PROBE,
            &FollowProbe::burnCall {
                rounds: U256::from(50_000 + i),
            },
        );
    }
    let first = follower.observe(&plan, At::Latest).await.unwrap();
    assert_eq!(first.path, Path::Multicall3);
    for (i, item) in first.items.iter().enumerate() {
        assert_eq!(
            item,
            &Item::Returned(
                U256::from(5 + 50_000 + i as u64)
                    .to_be_bytes::<32>()
                    .to_vec()
                    .into()
            ),
            "read {i}"
        );
    }
    assert_eq!(follower.capabilities(0).unwrap().max_reads, Some(1));
    // The next observation is split up front: the header and three calls.
    let next = follower.observe(&plan, At::Latest).await.unwrap();
    assert_eq!(next.items, first.items);
    assert_eq!(next.requests, 4);
}
