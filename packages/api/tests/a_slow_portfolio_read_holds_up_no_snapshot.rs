//! A portfolio is read from the chain, a read or two per vault, and a slow
//! node can take seconds over each. The record is in memory and the indexer
//! rewrites it every couple of seconds, so a portfolio request must not hold
//! the record while it reads the chain: if it did, `/api/snapshot`,
//! `/api/jobs` and the indexer's refresh of every chain would wait behind it.
//!
//! One vault on base is in the record. Base's node turns slow (10 s) for the
//! vault's `balanceOf` only, and someone asks for a portfolio on base. 3 s
//! later `/api/snapshot` is asked for, and is answered at once.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::post;

const VAULT: &str = "00000000000000000000000000000000000000be";

async fn a_node(slow: Arc<AtomicBool>) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |body: String| {
            let slow = slow.clone();
            async move {
                let id = body
                    .split("\"id\":")
                    .nth(1)
                    .map(|rest| {
                        rest.chars()
                            .take_while(|c| c.is_ascii_digit())
                            .collect::<String>()
                    })
                    .filter(|id| !id.is_empty())
                    .unwrap_or_else(|| "0".to_string());
                let lower = body.to_ascii_lowercase();
                if slow.load(Ordering::SeqCst)
                    && lower.contains("\"eth_call\"")
                    && lower.contains(VAULT)
                    && lower.contains("70a08231")
                {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
                let result = if body.contains("\"eth_chainId\"") {
                    "\"0x7a69\"".to_string()
                } else if body.contains("\"eth_blockNumber\"") {
                    "\"0x10\"".to_string()
                } else if body.contains("\"eth_getBlockByNumber\"") {
                    let mut block =
                        alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                    block.header.inner.number = 16;
                    block.header.inner.timestamp = 1_700_000_000;
                    serde_json::to_string(&block).unwrap()
                } else if body.contains("\"eth_call\"") {
                    // `vaults()` (any call whose input is the bare selector of
                    // `vaults()`) names the one vault; anything else is zeros.
                    let vaults = format!(
                        "{:x}",
                        alloy::primitives::FixedBytes::<4>::from(
                            <chain::bindings::factory::IVaultFactory::vaultsCall
                                as alloy::sol_types::SolCall>::SELECTOR
                        )
                    );
                    if lower.contains(&format!("\"0x{vaults}\"")) {
                        format!(
                            "\"0x{}{}{}\"",
                            "0".repeat(62) + "20",
                            "0".repeat(63) + "1",
                            "0".repeat(24) + VAULT
                        )
                    } else {
                        format!("\"0x{}\"", "0".repeat(64 * 128))
                    }
                } else if body.contains("\"eth_getBalance\"") {
                    "\"0x0\"".to_string()
                } else if body.contains("\"eth_getCode\"") {
                    "\"0x01\"".to_string()
                } else {
                    "[]".to_string()
                };
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

fn deployments() -> String {
    let one = "0x0000000000000000000000000000000000000001";
    let mut fields = vec![
        r#""chain_key":"evm:31337""#.to_string(),
        r#""chain_id":31337"#.to_string(),
        r#""rpc_url":"""#.to_string(),
        r#""deployed_block":0"#.to_string(),
    ];
    for name in [
        "usd",
        "venue",
        "attestation_verifier",
        "attestation",
        "publishing",
        "prices",
        "strategies",
        "reviews",
        "profiles",
        "factory",
        "station_factory",
        "driver",
        "bridge",
    ] {
        fields.push(format!(r#""{name}":"{one}""#));
    }
    let roles: Vec<String> = [
        "admin", "platform", "attestor", "builder", "reviewer", "executor", "keeper", "operator",
        "author", "viewer",
    ]
    .iter()
    .map(|role| format!(r#""{role}":"{one}""#))
    .collect();
    fields.push(format!(r#""roles":{{{}}}"#, roles.join(",")));
    format!("{{{}}}", fields.join(","))
}

struct Killed(std::process::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_portfolio_read_does_not_hold_up_the_snapshot() {
    let dir = std::env::temp_dir().join(format!(
        "shonin-api-slow-portfolio-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let deployments_dir = dir.join("deployments");
    std::fs::create_dir_all(&deployments_dir).unwrap();
    std::fs::create_dir_all(dir.join("store")).unwrap();
    let slow = Arc::new(AtomicBool::new(false));
    let base = a_node(slow.clone()).await;
    std::fs::write(
        dir.join("chains.toml"),
        format!(
            r#"
            [chains."evm:31337"]
            label = "Base (local)"
            rpc = "http://{base}"
            dollar = "USDC"

            [networks.local]
            base = "evm:31337"
            "#
        ),
    )
    .unwrap();
    std::fs::write(deployments_dir.join("evm-31337.json"), deployments()).unwrap();

    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let _api = Killed(
        std::process::Command::new(env!("CARGO_BIN_EXE_api"))
            .args(["--network", "local", "--vault-chain", "base", "--chains"])
            .arg(dir.join("chains.toml"))
            .arg("--deployments-dir")
            .arg(&deployments_dir)
            .arg("--store")
            .arg(dir.join("store"))
            .args([
                "--builder-url",
                "http://127.0.0.1:9",
                "--executor-url",
                "http://127.0.0.1:9",
                "--port",
                &port.to_string(),
            ])
            .env_remove("SHONIN_DEPLOYMENTS_JSON")
            .env_remove("SHONIN_VAULT_CHAIN")
            .env_remove("SHONIN_NETWORK")
            .env_remove("SHONIN_CHAINS")
            .env_remove("SHONIN_DEPLOYMENTS_DIR")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let snapshot_url = format!("http://127.0.0.1:{port}/api/snapshot");

    // The vault is in the record.
    let started = Instant::now();
    loop {
        assert!(
            started.elapsed() < Duration::from_secs(40),
            "the stand-in vault never reached the snapshot"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        let Ok(reply) = http.get(&snapshot_url).send().await else {
            continue;
        };
        let Ok(snapshot) = reply.json::<serde_json::Value>().await else {
            continue;
        };
        if snapshot["strategies"]
            .as_array()
            .is_some_and(|all| !all.is_empty())
        {
            break;
        }
    }

    // Base's node turns slow for the vault's balanceOf; someone asks for a
    // portfolio.
    slow.store(true, Ordering::SeqCst);
    let portfolio = format!(
        "http://127.0.0.1:{port}/api/base/portfolio/0x00000000000000000000000000000000000000a1"
    );
    let asking = http.clone();
    tokio::spawn(async move {
        let _ = asking.get(portfolio).send().await;
    });
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The record is in memory: it is answered at once.
    let asked = Instant::now();
    let reply = http.get(&snapshot_url).send().await.unwrap();
    let took = asked.elapsed();
    assert!(reply.status().is_success());
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        took < Duration::from_secs(2),
        "/api/snapshot took {took:?}: held up by one portfolio request's chain reads"
    );
}
