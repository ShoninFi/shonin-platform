//! The chain clock is bounded only while each recent observation's head is
//! within the lag bounds of the wall clock and chain time moves at the wall
//! clock's rate: a chain mined in real time settles after the configured
//! number of observations over a few seconds; a local node's fast clock and
//! an endpoint far behind never do. An unbounded chain marks every step for
//! the caller's active path.

mod stand_in_node;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chain::follow::{At, Plan, Unbounded};
use serde_json::json;
use stand_in_node::{Anvil, Proxy, policy};

fn now_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Mine one block stamped with the wall clock.
async fn mine_now(anvil: &Anvil) {
    anvil
        .rpc("evm_setNextBlockTimestamp", json!([now_s()]))
        .await
        .unwrap();
    anvil.mine(1, 0).await;
}

#[tokio::test]
async fn a_chain_mined_in_real_time_settles_and_a_fast_clock_never_does() {
    let anvil = Anvil::start(31_460_001, now_s() - 600).await;
    anvil.mine(300, 1).await;
    let mut follower = stand_in_node::follower(&[&anvil.url], policy()).await;
    let plan = Plan::default();
    let mut bounded = Vec::new();
    for _ in 0..4 {
        mine_now(&anvil).await;
        let step = follower.step(&plan, At::Latest).await;
        bounded.push(step.bounded);
        tokio::time::sleep(Duration::from_millis(2_600)).await;
    }
    // Three observations to settle, over at least five seconds.
    assert_eq!(
        bounded,
        vec![false, false, true, true],
        "{:?}",
        follower.clock.state()
    );
    let wake = follower.clock.to_wall_ms(now_s() + 10).unwrap();
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(wake > wall && wake < wall + 13_000);
    // An hour a block: the head runs ahead of the wall clock.
    anvil.mine(3, 3_600).await;
    let step = follower.step(&plan, At::Latest).await;
    assert!(!step.bounded);
    assert!(matches!(
        follower.clock.state(),
        Err(Unbounded::Ahead { .. })
    ));
    assert_eq!(follower.clock.to_wall_ms(now_s()), None);
}

#[tokio::test]
async fn an_endpoint_whose_head_is_old_leaves_the_chain_unbounded() {
    let anvil = Anvil::start(31_460_002, now_s() - 600).await;
    anvil.mine(300, 1).await;
    let proxy = Proxy::start(&anvil.url).await;
    // The node's head is stamped with the wall clock each round; the
    // endpoint is twenty blocks behind it, so its head is minutes old.
    proxy.script(|script| script.behind = 20);
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    let plan = Plan::default();
    for _ in 0..4 {
        mine_now(&anvil).await;
        let step = follower.step(&plan, At::Latest).await;
        assert!(step.observed.is_some());
        assert!(!step.bounded);
        tokio::time::sleep(Duration::from_millis(1_800)).await;
    }
    assert!(
        matches!(follower.clock.state(), Err(Unbounded::Behind { .. })),
        "{:?}",
        follower.clock.state()
    );
}
