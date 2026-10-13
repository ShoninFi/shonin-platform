//! An observation costs at most `read_budget_ms` on each endpoint it asks,
//! and a step one budget more, whatever the endpoints do: an endpoint that
//! never answers, first in the list; one whose capability check takes
//! longer than a budget (a slow endpoint that needs the hash-pin probe);
//! one that answers the observation and then stalls on the headers the
//! anchors need. The checks go on in the background and a later
//! observation uses what they found, without asking again.

mod stand_in_node;

use std::time::{Duration, Instant};

use chain::follow::{At, Continuity, Path, Plan};
use stand_in_node::{Anvil, Proxy, policy};

fn budgeted(ms: u64) -> chain::rpc_policy::RpcPolicy {
    let mut policy = policy();
    policy.read_budget_ms = ms;
    policy
}

#[tokio::test]
async fn a_silent_endpoint_first_in_the_list_costs_a_step_at_most_three_budgets() {
    let anvil = Anvil::start(31_490_005, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    let silent = Proxy::start(&anvil.url).await;
    let honest = Proxy::start(&anvil.url).await;
    silent.script(|script| script.lag = Some(Duration::from_secs(15)));
    let mut follower = stand_in_node::follower(&[&silent.url, &honest.url], budgeted(1_000)).await;
    let plan = Plan::default();
    for turn in 0..3 {
        let started = Instant::now();
        let step = follower.step(&plan, At::Latest).await;
        let took = started.elapsed();
        assert!(
            took <= Duration::from_millis(3 * 1_000 + 500),
            "step {turn} took {took:?} with two endpoints and a 1 s budget"
        );
        let observed = step
            .observed
            .unwrap_or_else(|| panic!("step {turn}: {:?}", step.continuity));
        assert_eq!(observed.endpoint, 1, "step {turn}");
        anvil.mine(1, 1).await;
    }
    // Once preferred, the honest endpoint is asked first and the silent one
    // is not asked at all.
    silent.forget_seen();
    let started = Instant::now();
    let step = follower.step(&plan, At::Latest).await;
    assert!(step.observed.is_some(), "{:?}", step.continuity);
    assert!(started.elapsed() < Duration::from_millis(1_000));
    assert_eq!(silent.seen().methods.values().sum::<u64>(), 0);
}

#[tokio::test]
async fn a_check_longer_than_a_budget_runs_on_and_a_later_observation_uses_it() {
    let anvil = Anvil::start(31_490_006, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    // No helper, no Multicall3: single reads, so the hash-pin probe (over a
    // dozen requests) is needed, and at 150 ms a request it takes longer
    // than the 1 s budget.
    let slow = Proxy::start(&anvil.url).await;
    slow.script(|script| {
        script.refuse_overrides = true;
        script.lag = Some(Duration::from_millis(150));
    });
    let mut follower = stand_in_node::follower(&[&slow.url], budgeted(1_000)).await;
    let plan = Plan::default();
    let mut succeeded = None;
    for attempt in 0..6 {
        let started = Instant::now();
        let result = follower.observe(&plan, At::Latest).await;
        let took = started.elapsed();
        assert!(
            took <= Duration::from_millis(1_000 + 400),
            "attempt {attempt} took {took:?} with a 1 s budget"
        );
        if let Ok(observed) = result {
            assert_eq!(observed.path, Path::Individual);
            succeeded = Some(attempt);
            break;
        }
    }
    let succeeded = succeeded.expect("an observation once the check landed");
    assert!(succeeded <= 4, "succeeded only at attempt {succeeded}");
    let known = follower.capabilities(0).unwrap();
    assert_eq!(known.overrides, Some(false));
    assert_eq!(known.multicall3, Some(false));
    assert_eq!(known.hash_pin, Some(true));
    // The probe ran once: the next observation is the header alone.
    slow.forget_seen();
    let observed = follower.observe(&plan, At::Latest).await.unwrap();
    assert_eq!(observed.requests, 1);
    assert_eq!(
        slow.seen().methods.values().sum::<u64>(),
        1,
        "{:?}",
        slow.seen().methods
    );
}

#[tokio::test]
async fn an_endpoint_that_stalls_on_headers_after_the_observation_costs_a_step_two_budgets() {
    let anvil = Anvil::start(31_490_007, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    let proxy = Proxy::start(&anvil.url).await;
    let mut follower = stand_in_node::follower(&[&proxy.url], budgeted(1_000)).await;
    let plan = Plan::default();
    let first = follower.step(&plan, At::Latest).await;
    assert!(first.observed.is_some(), "{:?}", first.continuity);
    // Anchors far below the window, so the next check needs header reads,
    // which the endpoint stalls on once the observation's call has gone
    // through: a node that answers one request and hangs on the next.
    anvil.mine(400, 1).await;
    proxy.forget_seen();
    let started = Instant::now();
    let step = {
        let step_future = follower.step(&plan, At::Latest);
        tokio::pin!(step_future);
        let mut stalled = false;
        loop {
            tokio::select! {
                result = &mut step_future => break result,
                _ = tokio::time::sleep(Duration::from_millis(5)), if !stalled => {
                    if proxy.seen().count("eth_call") >= 1 {
                        stalled = true;
                        proxy.script(|script| script.lag = Some(Duration::from_secs(15)));
                    }
                }
            }
        }
    };
    let took = started.elapsed();
    assert!(
        took <= Duration::from_millis(2 * 1_000 + 500),
        "the step took {took:?} with one endpoint and a 1 s budget"
    );
    if step.observed.is_some() {
        assert!(
            matches!(
                step.continuity,
                Continuity::Unknown(_) | Continuity::Extends
            ),
            "{:?}",
            step.continuity
        );
    }
}
