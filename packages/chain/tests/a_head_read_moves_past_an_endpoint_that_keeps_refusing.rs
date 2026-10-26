//! A head read is asked of the endpoint preferred, then the others in turn,
//! as an observation is: an endpoint that limits the rate of every request
//! for good does not hold the chain, the one that answers becomes the
//! preferred, and the observation pinned to the head it gave is read there.

mod stand_in_node;

use alloy::eips::BlockNumberOrTag;
use chain::follow::{At, Plan};
use stand_in_node::{Anvil, Proxy, policy};

#[tokio::test]
async fn a_head_read_moves_past_an_endpoint_that_keeps_refusing() {
    let anvil = Anvil::start(31_480_011, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    let refusing = Proxy::start(&anvil.url).await;
    let honest = Proxy::start(&anvil.url).await;
    let mut follower = stand_in_node::follower(&[&refusing.url, &honest.url], policy()).await;
    // From here on the first endpoint turns every request away, as one whose
    // plan's capacity is spent does.
    refusing.script(|script| {
        script.turn_away_next = u32::MAX;
        script.turn_away_with = 429;
    });
    assert_eq!(follower.preferred(), 0);
    // Asked of the preferred endpoint alone, the head is never read.
    assert!(
        follower
            .header(follower.preferred(), BlockNumberOrTag::Latest)
            .await
            .is_err()
    );

    let head = follower
        .header_any(BlockNumberOrTag::Latest)
        .await
        .expect("the second endpoint gives the head");
    assert_eq!(head.number, anvil.head().await);
    assert_eq!(follower.preferred(), 1);

    // The observation pinned to that head is read, on the endpoint that gave
    // it, at that block.
    let observed = follower
        .observe(&Plan::default(), At::Pinned(head))
        .await
        .expect("the chain is read");
    assert_eq!(observed.endpoint, 1);
    assert_eq!(observed.number, head.number);
    assert_eq!(observed.hash, Some(head.hash));

    // The next head read starts from the endpoint that answered: the
    // refusing one is not asked first again.
    let refused = refusing.seen().count("eth_getBlockByNumber");
    anvil.mine(1, 1).await;
    let next = follower.header_any(BlockNumberOrTag::Latest).await.unwrap();
    assert_eq!(next.number, head.number + 1);
    assert_eq!(refusing.seen().count("eth_getBlockByNumber"), refused);

    // A header at a past block is read the same way.
    let past = follower
        .header_any(BlockNumberOrTag::Number(head.number - 10))
        .await
        .unwrap();
    assert_eq!(past.number, head.number - 10);
}

#[tokio::test]
async fn a_head_read_every_endpoint_refuses_fails_naming_each() {
    let anvil = Anvil::start(31_480_012, 1_700_000_000).await;
    anvil.mine(10, 1).await;
    let first = Proxy::start(&anvil.url).await;
    let second = Proxy::start(&anvil.url).await;
    let mut follower = stand_in_node::follower(&[&first.url, &second.url], policy()).await;
    for proxy in [&first, &second] {
        proxy.script(|script| {
            script.turn_away_next = u32::MAX;
            script.turn_away_with = 429;
        });
    }
    let error = follower
        .header_any(BlockNumberOrTag::Latest)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("endpoint 1:"), "{error}");
    assert!(error.contains("endpoint 2:"), "{error}");
    assert_eq!(follower.preferred(), 0);
}
