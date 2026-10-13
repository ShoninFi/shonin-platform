//! An endpoint that accepts requests and does not answer them costs an
//! observation at most its read budget before the follower moves on or
//! gives up. The keeper's slot arithmetic (two observations in every
//! window of opportunity) counts `read_budget_ms` as the time an observation
//! may take, so a stall many times longer, on every observation that reaches
//! that endpoint, is a missed window. Finding out what the endpoint can do is
//! part of that budget, and a check it never answered is not asked again on
//! every observation.

mod stand_in_node;

use std::time::{Duration, Instant};

use chain::follow::{At, Plan, capabilities};
use stand_in_node::{Anvil, Proxy, policy};

#[tokio::test]
async fn an_endpoint_that_does_not_answer_costs_an_observation_at_most_its_read_budget() {
    let anvil = Anvil::start(31_480_002, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    let honest = Proxy::start(&anvil.url).await;
    let silent = Proxy::start(&anvil.url).await;
    let mut policy = policy();
    policy.read_budget_ms = 1_000;
    let mut follower = stand_in_node::follower(&[&honest.url, &silent.url], policy).await;
    let plan = Plan::default();
    // The honest endpoint answers first and is preferred from then on.
    let first = follower.observe(&plan, At::Latest).await.unwrap();
    assert_eq!(first.endpoint, 0);
    // The other endpoint takes every request and answers long after any
    // budget. Each time the honest one fails a request, the follower turns
    // to it: with two endpoints an observation costs at most two budgets,
    // not the capability checks' timeouts over and over.
    silent.script(|script| script.lag = Some(Duration::from_secs(15)));
    silent.forget_seen();
    let budget = follower.policy().read_budget_ms;
    let bound = Duration::from_millis(2 * budget + 2_000);
    for round in 1..=3 {
        if round == 3 {
            // Once the check has given up, the endpoint is not asked again
            // for a while: the next observation does not wait on it at all.
            tokio::time::sleep(capabilities::CHECK_TIMEOUT + Duration::from_millis(500)).await;
        }
        honest.script(|script| script.fail_next = 1);
        let started = Instant::now();
        let _ = follower.observe(&plan, At::Latest).await;
        let took = started.elapsed();
        assert!(
            took <= bound,
            "observation {round} took {took:?} with read_budget_ms={budget} and two endpoints",
        );
        if round == 3 {
            assert!(
                took < Duration::from_millis(budget),
                "observation {round} took {took:?}, waiting on an endpoint whose check gave up"
            );
        }
        // One check was started, and later observations waited on it, or
        // passed the endpoint over, rather than asking again.
        assert_eq!(
            silent.seen().methods.values().sum::<u64>(),
            1,
            "{:?}",
            silent.seen().methods
        );
    }
}
