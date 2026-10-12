//! A result whose own block (number, timestamp, parent hash) differs from
//! the block asked for is never accepted, on any path: an endpoint that
//! answers every pin at its head (as some HyperEVM endpoints do), or one
//! behind the block, gives no observation, and the next endpoint gives the
//! right one. Such an endpoint is never trusted with single pinned reads.

mod stand_in_node;

use chain::follow::{At, Header, Path, Plan};
use stand_in_node::{Anvil, Proxy, policy};

async fn header(anvil: &Anvil, number: u64) -> Header {
    let (hash, parent_hash, timestamp) = anvil.block(number).await;
    Header {
        number,
        hash,
        parent_hash,
        timestamp,
    }
}

fn plan() -> Plan {
    let mut plan = Plan::default();
    plan.read(
        stand_in_node::PROBE,
        &stand_in_node::FollowProbe::valueCall {},
    );
    plan.read(
        stand_in_node::PROBE,
        &stand_in_node::FollowProbe::contextCall {},
    );
    plan
}

#[tokio::test]
async fn an_endpoint_that_answers_pins_at_its_head_gives_no_pinned_observation() {
    let anvil = Anvil::start(31_430_001, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    stand_in_node::put_probe(&anvil, stand_in_node::PROBE, 9).await;
    anvil
        .set_code(
            stand_in_node::MULTICALL,
            &stand_in_node::code("StandInMulticall3"),
        )
        .await;
    anvil.mine(5, 1).await;
    let ignoring = Proxy::start(&anvil.url).await;
    ignoring.script(|script| script.ignore_pins = true);
    let mut follower = stand_in_node::follower(&[&ignoring.url], policy()).await;
    let asked = header(&anvil, 300).await;
    // The helper, pinned by hash and then by number, runs at the head.
    let refused = follower.observe(&plan(), At::Pinned(asked)).await;
    assert!(refused.is_err(), "{refused:?}");
    // The same through Multicall3 when overrides are refused too.
    ignoring.script(|script| script.refuse_overrides = true);
    let mut follower = stand_in_node::follower(&[&ignoring.url], policy()).await;
    let refused = follower.observe(&plan(), At::Pinned(asked)).await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(follower.capabilities(0).unwrap().multicall3, Some(true));
    // Without Multicall3 there is no path at all: the endpoint answers an
    // unknown hash, so it is never shown to honour hash pins.
    let mut follower =
        chain::follow::Follower::new(stand_in_node::connect(&[&ignoring.url]).await, policy());
    let refused = follower.observe(&plan(), At::Latest).await;
    assert!(refused.is_err(), "{refused:?}");
    let refused = follower.observe(&plan(), At::Latest).await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(follower.capabilities(0).unwrap().hash_pin, Some(false));
}

#[tokio::test]
async fn the_next_endpoint_gives_the_block_asked_for() {
    let anvil = Anvil::start(31_430_002, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    stand_in_node::put_probe(&anvil, stand_in_node::PROBE, 9).await;
    anvil.mine(5, 1).await;
    let ignoring = Proxy::start(&anvil.url).await;
    ignoring.script(|script| script.ignore_pins = true);
    let behind = Proxy::start(&anvil.url).await;
    behind.script(|script| script.behind = 3);
    let honest = Proxy::start(&anvil.url).await;
    let mut follower =
        stand_in_node::follower(&[&ignoring.url, &behind.url, &honest.url], policy()).await;
    let head = anvil.head().await;
    // An older block, which the endpoint behind has, and the head's own,
    // which it has not reached; a block is mined after the header is read,
    // so the endpoint that answers at its head is never at the block asked.
    for (number, endpoint) in [(300, 1), (head, 2)] {
        let asked = header(&anvil, number).await;
        anvil.mine(1, 1).await;
        let observed = follower.observe(&plan(), At::Pinned(asked)).await.unwrap();
        assert_eq!(observed.endpoint, endpoint);
        assert_eq!(observed.path, Path::ViewReads);
        assert_eq!(
            (
                observed.number,
                observed.timestamp,
                observed.parent_hash,
                observed.hash
            ),
            (
                asked.number,
                asked.timestamp,
                asked.parent_hash,
                Some(asked.hash)
            )
        );
        let (number, _) = observed
            .decode::<stand_in_node::FollowProbe::contextCall>(1)
            .map(|context| (context.number, context.timestamp))
            .unwrap();
        assert_eq!(number, alloy::primitives::U256::from(asked.number));
        // The next observation starts from the endpoint that answered.
        assert_eq!(follower.preferred(), endpoint);
        follower =
            stand_in_node::follower(&[&ignoring.url, &behind.url, &honest.url], policy()).await;
    }
}
