//! An endpoint that turns requests away for a while, with a rate limit
//! (HTTP 429) or a server error (HTTP 503), has answered: it is there, and
//! may answer the next request. A capability check it cuts short that way
//! has learned nothing, and is asked again the next time it is wanted, so
//! the endpoint gives an observation as soon as it answers again, rather
//! than being passed over as a silent endpoint is (for a minute). A recheck
//! it cuts short keeps what was known.

mod stand_in_node;

use std::time::{Duration, Instant};

use alloy::primitives::U256;
use chain::follow::{At, Item, Path, Plan};
use stand_in_node::{Anvil, FollowProbe, PROBE, Proxy, policy};

/// How many requests the endpoint turns away: more than two checks ask
/// before they give up for want of an answer, so more than one observation
/// is tried before one succeeds.
const TURNED_AWAY: u32 = 8;

/// Well under the minute a silent endpoint is passed over for.
const WITHIN: Duration = Duration::from_secs(10);

async fn chain(id: u64) -> (Anvil, Proxy) {
    let anvil = Anvil::start(id, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    stand_in_node::put_probe(&anvil, PROBE, 5).await;
    anvil.mine(3, 1).await;
    let proxy = Proxy::start(&anvil.url).await;
    (anvil, proxy)
}

fn plan() -> Plan {
    let mut plan = Plan::default();
    plan.read(PROBE, &FollowProbe::valueCall {});
    plan
}

fn five() -> Item {
    Item::Returned(U256::from(5).to_be_bytes::<32>().to_vec().into())
}

/// The endpoint turns away the first requests the follower makes of it
/// with `status`; observations are tried until one succeeds.
async fn an_endpoint_turning_away_its_first_requests_gives_an_observation(id: u64, status: u16) {
    let (_anvil, proxy) = chain(id).await;
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    proxy.script(|script| {
        script.turn_away_next = TURNED_AWAY;
        script.turn_away_with = status;
    });
    let started = Instant::now();
    let mut attempts = 0;
    let observed = loop {
        attempts += 1;
        match follower.observe(&plan(), At::Latest).await {
            Ok(observed) => break observed,
            Err(error) => {
                assert!(
                    started.elapsed() < WITHIN,
                    "no observation after {attempts} attempts in {:?}: {error:#}",
                    started.elapsed()
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    };
    let took = started.elapsed();
    eprintln!("an observation at attempt {attempts}, after {took:?}");
    assert!(took < WITHIN, "the observation took {took:?}");
    assert_eq!(
        proxy.script.lock().unwrap().turn_away_next,
        0,
        "every request the endpoint turned away was asked"
    );
    assert_eq!(observed.path, Path::ViewReads);
    assert_eq!(observed.items, vec![five()]);
    let known = follower.capabilities(0).unwrap();
    assert_eq!(known.overrides, Some(true));
    assert!(known.source.is_some());
    // And the next observation goes straight to the endpoint.
    proxy.forget_seen();
    let next = follower.observe(&plan(), At::Latest).await.unwrap();
    assert_eq!(next.items, vec![five()]);
}

#[tokio::test]
async fn a_check_a_rate_limit_cuts_short_is_asked_again_when_next_wanted() {
    an_endpoint_turning_away_its_first_requests_gives_an_observation(31_490_011, 429).await;
}

#[tokio::test]
async fn a_check_a_server_error_cuts_short_is_asked_again_when_next_wanted() {
    an_endpoint_turning_away_its_first_requests_gives_an_observation(31_490_012, 503).await;
}

#[tokio::test]
async fn a_recheck_a_rate_limit_cuts_short_keeps_what_was_known() {
    let (_anvil, proxy) = chain(31_490_013).await;
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    let first = follower.observe(&plan(), At::Latest).await.unwrap();
    assert_eq!(first.path, Path::ViewReads);
    let before = follower.capabilities(0).unwrap();
    // An hour on, the next observation starts a recheck, which the endpoint
    // turns away along with the observation's own requests.
    tokio::time::pause();
    tokio::time::advance(chain::follow::capabilities::RECHECK_AFTER + Duration::from_secs(1)).await;
    tokio::time::resume();
    proxy.script(|script| {
        script.turn_away_next = u32::MAX;
        script.turn_away_with = 429;
    });
    proxy.forget_seen();
    let turned_away = follower.observe(&plan(), At::Latest).await;
    assert!(turned_away.is_err(), "{turned_away:?}");
    // The recheck runs on in the background; give it time to be turned
    // away too.
    let stop = Instant::now() + WITHIN;
    while proxy.seen().count("eth_getBlockByNumber") == 0 {
        assert!(
            Instant::now() < stop,
            "no recheck: {:?}",
            proxy.seen().methods
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = follower.capabilities(0).unwrap();
    assert_eq!(after.overrides, before.overrides);
    assert_eq!(after.source, before.source);
    // As soon as it answers again, it is used as before, with no check
    // waited for.
    proxy.script(|script| script.turn_away_next = 0);
    let started = Instant::now();
    let next = follower.observe(&plan(), At::Latest).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(next.path, Path::ViewReads);
    assert_eq!(next.items, vec![five()]);
}
