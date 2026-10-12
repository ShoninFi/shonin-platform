//! A replacement is reported only when an observation numbered above the
//! followed head proves a different hash at a height the follower holds,
//! and then at the exact fork point (or `Deep` past the anchors):
//! - an endpoint behind (a lower head) is `Lagging`, never `Replaced`;
//! - an endpoint on another fork at an equal or lower head is `Lagging`;
//! - an error, a timeout or a failed header read is `Unknown`, never
//!   `Replaced`, and changes nothing held;
//! - a fork d blocks below the newest anchor is `Replaced { fork_at, d }`
//!   for every d up to 256, whether the new head is just above or far
//!   above, and `Deep` at 257.

mod stand_in_node;

use std::time::Duration;

use chain::follow::{At, Continuity, Plan};
use serde_json::json;
use stand_in_node::{Anvil, Proxy, policy};

const GENESIS: u64 = 1_700_000_000;

#[tokio::test]
async fn a_lower_or_equal_head_is_never_a_replacement() {
    let anvil = Anvil::start(31_420_001, GENESIS).await;
    anvil.mine(300, 1).await;
    let (honest, behind) = (
        Proxy::start(&anvil.url).await,
        Proxy::start(&anvil.url).await,
    );
    let mut follower = stand_in_node::follower(&[&honest.url, &behind.url], policy()).await;
    let plan = Plan::default();
    let first = follower.observe_on(0, &plan, At::Latest).await.unwrap();
    assert_eq!(follower.check(&first).await, Continuity::Extends);
    let held: Vec<_> = follower.anchors.iter().collect();
    for lag in [1u64, 2, 5, 40, 299] {
        behind.script(|script| script.behind = lag);
        let observed = follower.observe_on(1, &plan, At::Latest).await.unwrap();
        assert_eq!(observed.number, 300 - lag);
        assert_eq!(
            follower.check(&observed).await,
            Continuity::Lagging {
                number: 300 - lag,
                head: 300
            }
        );
        assert_eq!(follower.anchors.iter().collect::<Vec<_>>(), held);
        assert_eq!(follower.anchors.head(), Some(300));
    }
    // The same head again, from either endpoint, extends.
    behind.script(|script| script.behind = 0);
    for endpoint in [0, 1] {
        let again = follower
            .observe_on(endpoint, &plan, At::Latest)
            .await
            .unwrap();
        assert_eq!(follower.check(&again).await, Continuity::Extends);
    }
}

#[tokio::test]
async fn an_endpoint_on_another_fork_replaces_only_from_above_and_at_the_exact_fork() {
    let one = Anvil::start(31_420_002, GENESIS).await;
    one.mine(300, 1).await;
    // A second node forked from the first at block 300: the same blocks
    // up to it, its own after.
    let two = ForkedAnvil::start(&one.url, 300).await;
    one.mine(10, 1).await;
    two.rpc("anvil_mine", json!(["0xc", "0x2"])).await.unwrap();
    let mut follower = stand_in_node::follower(&[&one.url, &two.url], policy()).await;
    let plan = Plan::default();
    let on_one = follower.observe_on(0, &plan, At::Latest).await.unwrap();
    assert_eq!(on_one.number, 310);
    assert_eq!(follower.check(&on_one).await, Continuity::Extends);
    // The other fork, two blocks longer: replaced above block 300, whose
    // anchor is 9 blocks below the newest (309).
    let on_two = follower.observe_on(1, &plan, At::Latest).await.unwrap();
    assert_eq!(on_two.number, 312);
    assert_eq!(
        follower.check(&on_two).await,
        Continuity::Replaced {
            fork_at: 300,
            depth: 9
        }
    );
    // The first fork, now lower, and then equal: never a replacement.
    let lower = follower.observe_on(0, &plan, At::Latest).await.unwrap();
    assert!(matches!(
        follower.check(&lower).await,
        Continuity::Lagging {
            number: 310,
            head: 312
        }
    ));
    one.mine(2, 1).await;
    let equal = follower.observe_on(0, &plan, At::Latest).await.unwrap();
    assert_eq!(equal.number, 312);
    assert!(matches!(
        follower.check(&equal).await,
        Continuity::Lagging {
            number: 312,
            head: 312
        }
    ));
    // Once it is longer, it replaces back, at the same fork.
    one.mine(3, 1).await;
    let longer = follower.observe_on(0, &plan, At::Latest).await.unwrap();
    assert_eq!(
        follower.check(&longer).await,
        Continuity::Replaced {
            fork_at: 300,
            depth: 11
        }
    );
}

#[tokio::test]
async fn an_error_or_a_timeout_is_never_a_replacement_and_changes_nothing() {
    let anvil = Anvil::start(31_420_003, GENESIS).await;
    anvil.mine(290, 1).await;
    let at_290 = anvil.snapshot().await;
    anvil.mine(10, 1).await;
    let proxy = Proxy::start(&anvil.url).await;
    let mut policy = policy();
    policy.read_budget_ms = 500;
    let mut follower = stand_in_node::follower(&[&proxy.url], policy).await;
    let plan = Plan::default();
    let step = follower.step(&plan, At::Latest).await;
    assert_eq!(step.continuity, Continuity::Extends);
    let held: Vec<_> = follower.anchors.iter().collect();
    // Requests fail.
    proxy.script(|script| script.fail_next = 1);
    let step = follower.step(&plan, At::Latest).await;
    assert!(
        matches!(step.continuity, Continuity::Unknown(_)),
        "{:?}",
        step.continuity
    );
    // Answers come after the budget.
    proxy.script(|script| script.lag = Some(Duration::from_millis(800)));
    let step = follower.step(&plan, At::Latest).await;
    assert!(
        matches!(step.continuity, Continuity::Unknown(_)),
        "{:?}",
        step.continuity
    );
    proxy.script(|script| script.lag = None);
    assert_eq!(follower.anchors.iter().collect::<Vec<_>>(), held);
    // A real replacement far above the window, while every header read
    // fails: unknown, not replaced, until the headers can be read.
    anvil.fork_at(&at_290).await;
    anvil.mine(400, 2).await;
    proxy.script(|script| script.fail_methods = vec!["eth_getBlockByNumber".into()]);
    let observed = follower.observe(&plan, At::Latest).await.unwrap();
    assert!(matches!(
        follower.check(&observed).await,
        Continuity::Unknown(_)
    ));
    assert_eq!(follower.anchors.iter().collect::<Vec<_>>(), held);
    proxy.script(|script| script.fail_methods.clear());
    let step = follower.step(&plan, At::Latest).await;
    assert_eq!(
        step.continuity,
        Continuity::Replaced {
            fork_at: 290,
            depth: 9
        }
    );
}

#[tokio::test]
async fn the_fork_point_is_exact_to_256_blocks_and_deep_beyond() {
    let anvil = Anvil::start(31_420_004, GENESIS).await;
    anvil.mine(400, 1).await;
    let proxy = Proxy::start(&anvil.url).await;
    let mut start = anvil.snapshot().await;
    for depth in [1u64, 2, 31, 255, 256, 257] {
        for above in [3u64, 300] {
            // The same 701 blocks each time, with a snapshot at the fork.
            let fork = 700 - depth;
            anvil.mine(fork - 400, 1).await;
            let at_fork = anvil.snapshot().await;
            anvil.mine(701 - fork, 1).await;
            assert_eq!(anvil.head().await, 701);
            let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
            let plan = Plan::default();
            // Held: the 257 blocks 444..=700, newest 700.
            let first = follower
                .observe(&plan, At::Pinned(stand_in_header(&anvil, 700).await))
                .await
                .unwrap();
            assert_eq!(follower.check(&first).await, Continuity::Extends);
            assert_eq!(
                follower.step(&plan, At::Latest).await.continuity,
                Continuity::Extends
            );
            assert_eq!(follower.anchors.newest().unwrap().0, 700);
            // Replace every block above the fork with blocks of another
            // pace, ending `above` blocks past the followed head (701).
            anvil.fork_at(&at_fork).await;
            anvil.mine(701 - fork + above, 2).await;
            proxy.forget_seen();
            let step = follower.step(&plan, At::Latest).await;
            if depth <= 256 {
                assert_eq!(
                    step.continuity,
                    Continuity::Replaced {
                        fork_at: fork,
                        depth
                    },
                    "depth {depth}, {above} above"
                );
            } else {
                assert_eq!(
                    step.continuity,
                    Continuity::Deep,
                    "depth {depth}, {above} above"
                );
            }
            // Headers only where the window does not reach, by halves.
            let headers = proxy.seen().count("eth_getBlockByNumber");
            assert!(
                headers <= 10,
                "{headers} headers at depth {depth}, {above} above"
            );
            if above == 3 && depth < 250 {
                assert_eq!(headers, 0, "depth {depth}");
            }
            anvil.rpc("evm_revert", json!([start])).await.unwrap();
            start = anvil.snapshot().await;
        }
    }
}

async fn stand_in_header(anvil: &Anvil, number: u64) -> chain::follow::Header {
    let (hash, parent_hash, timestamp) = anvil.block(number).await;
    chain::follow::Header {
        number,
        hash,
        parent_hash,
        timestamp,
    }
}

/// An anvil forked from another at a block, stopped when dropped.
struct ForkedAnvil {
    child: std::process::Child,
    url: String,
}

impl Drop for ForkedAnvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ForkedAnvil {
    async fn start(from: &str, block: u64) -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = std::process::Command::new("anvil")
            .args([
                "--silent",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--no-mining",
                "--fork-url",
                from,
                "--fork-block-number",
                &block.to_string(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("anvil is required for the follower's tests");
        let forked = Self {
            child,
            url: format!("http://127.0.0.1:{port}"),
        };
        let stop = std::time::Instant::now() + Duration::from_secs(20);
        while forked.rpc("eth_blockNumber", json!([])).await.is_err() {
            assert!(
                std::time::Instant::now() < stop,
                "the forked anvil did not start"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        forked
    }

    async fn rpc(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        stand_in_node::rpc(&self.url, method, params).await
    }
}
