//! A chain whose head keeps within `head_lag_max_ms` of the wall clock is
//! read at its wakes, from one observation each; one whose head lags beyond
//! it (an endpoint behind, a clock skewed) is read every round, of the chain
//! itself, as the keeper always has. A local chain mining in real time with
//! its clock set back stands in for a lagging endpoint.
mod process;

use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use process::{FACTORY, start_keeper_configured, wait_until};

/// A factory with no vaults, that replaces none, compiled from:
///
/// ```solidity
/// contract F {
///     function vaults() external pure returns (address[] memory listed) {}
///     function previous() external pure returns (address) {}
/// }
/// ```
const NO_VAULTS: &str = "608060405234801561001057600080fd5b50600436106100365760003560e01c80637c2b2e711461003b5780638220ef5b14610059575b600080fd5b610043610077565b60405161005091906100c2565b60405180910390f35b61006161007c565b60405161006e919061019b565b60405180910390f35b600090565b606090565b600073ffffffffffffffffffffffffffffffffffffffff82169050919050565b60006100ac82610081565b9050919050565b6100bc816100a1565b82525050565b60006020820190506100d760008301846100b3565b92915050565b600081519050919050565b600082825260208201905092915050565b6000819050602082019050919050565b610112816100a1565b82525050565b60006101248383610109565b60208301905092915050565b6000602082019050919050565b6000610148826100dd565b61015281856100e8565b935061015d836100f9565b8060005b8381101561018e5781516101758882610118565b975061018083610130565b925050600181019050610161565b5085935050505092915050565b600060208201905081810360008301526101b5818461013d565b90509291505056fea2646970667358221220be6b0eb68d570bd4018a5b3c6a74edf0fbca310efe9fefa3c5cd26de67f444d764736f6c634300081d0033";

/// A local chain mining a block every second, its clock `behind` seconds
/// behind the wall clock; stopped when dropped.
struct Chain(Child, String);

impl Drop for Chain {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn a_chain_behind_by(behind: u64) -> Chain {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let child = Command::new("anvil")
        .args([
            "--port",
            &port.to_string(),
            "--chain-id",
            "31337",
            "--block-time",
            "1",
            "--timestamp",
            &(now - behind).to_string(),
            "--silent",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("anvil runs");
    let url = format!("http://127.0.0.1:{port}");
    let asked = |method: &str, params: serde_json::Value| serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let http = reqwest::Client::new();
    for _ in 0..100 {
        if http
            .post(&url)
            .json(&asked("eth_chainId", serde_json::json!([])))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    http.post(&url)
        .json(&asked(
            "anvil_setCode",
            serde_json::json!([FACTORY, format!("0x{NO_VAULTS}")]),
        ))
        .send()
        .await
        .unwrap();
    // A chain only just started has too few blocks for the follower's
    // checks of what the endpoint can do; it is given a few first.
    for _ in 0..200 {
        let head = http
            .post(&url)
            .json(&asked("eth_blockNumber", serde_json::json!([])))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        let number = head["result"]
            .as_str()
            .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        if number >= 8 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Chain(child, url)
}

/// What the keeper said of how it reads the chain, in order.
fn paths(logged: &str) -> Vec<&'static str> {
    logged
        .lines()
        .filter_map(|line| {
            if line.contains("the chain is read at its wakes") {
                Some("wakes")
            } else if line.contains("the chain is read every round") {
                Some("every round")
            } else {
                None
            }
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chain_is_read_at_its_wakes_while_its_head_keeps_within_its_lag_bound() {
    // A bound of 2.5 s: a head 1 s behind is 1 to 2 s old when read.
    let chain = a_chain_behind_by(1).await;
    let keeper = start_keeper_configured(
        "lag-inside",
        &[("base", 31337, &chain.1)],
        "http://127.0.0.1:1",
        &[],
        &[("head_lag_max_ms", 2_500)],
    );
    assert!(
        wait_until(Duration::from_secs(30), || paths(&keeper.logged())
            .contains(&"wakes"))
        .await,
        "a chain within its lag bound was never read at its wakes: {}",
        keeper.logged()
    );
    // And it stays there.
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert_eq!(
        paths(&keeper.logged()).last(),
        Some(&"wakes"),
        "{}",
        keeper.logged()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chain_whose_head_lags_beyond_its_bound_is_read_every_round() {
    // A head 3 s behind is 3 to 4 s old when read: past a 2.5 s bound.
    let chain = a_chain_behind_by(3).await;
    let keeper = start_keeper_configured(
        "lag-outside",
        &[("base", 31337, &chain.1)],
        "http://127.0.0.1:1",
        &[],
        &[("head_lag_max_ms", 2_500)],
    );
    assert!(
        wait_until(Duration::from_secs(20), || !paths(&keeper.logged())
            .is_empty())
        .await,
        "{}",
        keeper.logged()
    );
    tokio::time::sleep(Duration::from_secs(15)).await;
    let said = paths(&keeper.logged());
    assert!(
        !said.contains(&"wakes"),
        "a chain lagging beyond its bound was read at its wakes: {}",
        keeper.logged()
    );
    assert!(
        keeper.logged().contains("is not bounded"),
        "{}",
        keeper.logged()
    );
}
