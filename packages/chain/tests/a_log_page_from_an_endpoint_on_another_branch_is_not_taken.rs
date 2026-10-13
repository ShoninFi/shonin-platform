//! A page of logs is taken only from the branch the observation proved. When
//! log queries go to endpoints of their own (`logs_rpc`), a page answered
//! by an endpoint on another branch carries that branch's logs, and a log
//! the followed branch has but the other has not is simply absent from it.
//! Such a page must not be taken as the followed branch's logs: each page's
//! end must be shown to be on the followed branch, or the read must say it
//! could not check. A logs endpoint on the followed branch is still read,
//! at one header a window.

mod stand_in_node;

use std::time::Duration;

use alloy::primitives::U256;
use alloy_sol_types::{SolCall, SolEvent};
use chain::follow::{Follower, Options, RangeRead, RangeReader, Watch};
use serde_json::json;
use stand_in_node::{Anvil, FollowProbe, PROBE, Proxy, policy};

/// A follower observing through `rpc` and reading logs through `logs`.
async fn follower(rpc: &str, logs: &str) -> Follower {
    let mut config = stand_in_node::config(&[rpc]);
    config.logs_rpc = Some(logs.to_string());
    let client = chain::Client::connect(&config, chain::Signing::ReadOnly)
        .await
        .unwrap();
    Follower::with_options(
        client,
        policy(),
        Options {
            multicall3: stand_in_node::MULTICALL,
            multicall3_code_hash: stand_in_node::code_hash("StandInMulticall3"),
        },
    )
}

fn watch() -> Watch {
    Watch {
        addresses: vec![PROBE],
        topics: vec![FollowProbe::Noted::SIGNATURE_HASH],
    }
}

#[tokio::test]
async fn a_log_page_from_an_endpoint_on_another_branch_is_not_taken() {
    let one = Anvil::start(31_480_003, 1_700_000_000).await;
    one.mine(300, 1).await;
    stand_in_node::put_probe(&one, PROBE, 1).await;
    one.mine(10, 1).await;
    let fork = one.head().await;
    // A second node forked at `fork`: the same blocks up to it, its own
    // after. The followed branch gets a log in the block after the fork;
    // the other branch gets none, and both reach the same height.
    let two = ForkedAnvil::start(&one.url, fork).await;
    one.send(
        PROBE,
        FollowProbe::noteCall {
            count: U256::from(1),
        }
        .abi_encode(),
    )
    .await;
    let noted_at = one.head().await;
    assert_eq!(noted_at, fork + 1);
    one.mine(48, 1).await;
    two.rpc("anvil_mine", json!(["0x31", "0x1"])).await.unwrap();
    assert_eq!(one.head().await, two.head().await);
    assert_ne!(
        one.block(noted_at).await.0,
        two.block(noted_at).await.0,
        "the two nodes are on different branches above the fork"
    );

    // Observations go to the first node, log queries to the second.
    let mut follower = follower(&one.url, &two.url).await;
    let mut reader = RangeReader::new(watch(), 1, &policy());
    match reader.read(&mut follower, None).await {
        RangeRead::Read { logs, through, .. } => {
            assert!(
                logs.iter().any(|log| log.block_number == Some(noted_at)),
                "the read says every block through {through} was read on the followed branch, \
                 and has no log for block {noted_at}, which that branch has: {logs:?}"
            );
        }
        // Not checked, nothing taken: acceptable.
        RangeRead::Unverified(_) => {}
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_log_page_from_a_logs_endpoint_on_the_followed_branch_is_taken_at_one_header_a_window() {
    let one = Anvil::start(31_480_005, 1_700_000_000).await;
    one.mine(300, 1).await;
    stand_in_node::put_probe(&one, PROBE, 1).await;
    one.send(
        PROBE,
        FollowProbe::noteCall {
            count: U256::from(1),
        }
        .abi_encode(),
    )
    .await;
    let noted_at = one.head().await;
    one.mine(48, 1).await;
    // Log queries go to an endpoint of their own, on the same node.
    let logs = Proxy::start(&one.url).await;
    let mut follower = follower(&one.url, &logs.url).await;
    let mut reader = RangeReader::new(watch(), 1, &policy());
    logs.forget_seen();
    match reader.read(&mut follower, None).await {
        RangeRead::Read {
            logs: read,
            through,
            caught_up,
        } => {
            assert!(caught_up);
            assert_eq!(through, one.head().await - 1);
            assert_eq!(
                read.iter().map(|log| log.block_number).collect::<Vec<_>>(),
                vec![Some(noted_at)]
            );
        }
        other => panic!("{other:?}"),
    }
    // One window, so one header on the endpoint that answered it.
    let seen = logs.seen();
    assert_eq!(seen.count("eth_getLogs"), 1, "{:?}", seen.methods);
    assert_eq!(seen.count("eth_getBlockByNumber"), 1, "{:?}", seen.methods);
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

    async fn head(&self) -> u64 {
        stand_in_node::quantity(&self.rpc("eth_blockNumber", json!([])).await.unwrap())
    }

    async fn block(&self, number: u64) -> (alloy::primitives::B256, alloy::primitives::B256, u64) {
        let block = self
            .rpc(
                "eth_getBlockByNumber",
                json!([format!("0x{number:x}"), false]),
            )
            .await
            .unwrap();
        (
            block["hash"].as_str().unwrap().parse().unwrap(),
            block["parentHash"].as_str().unwrap().parse().unwrap(),
            stand_in_node::quantity(&block["timestamp"]),
        )
    }
}
