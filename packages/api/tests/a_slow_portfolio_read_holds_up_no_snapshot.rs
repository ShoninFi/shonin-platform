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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::post;

const VAULT: &str = "00000000000000000000000000000000000000be";

fn block_hash(number: u64) -> alloy::primitives::B256 {
    let mut hash = [0x44; 32];
    hash[24..].copy_from_slice(&number.to_be_bytes());
    alloy::primitives::B256::from(hash)
}

async fn a_node(
    slow: Arc<AtomicBool>,
    started_reads: Arc<AtomicU64>,
    finished_reads: Arc<AtomicU64>,
) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |body: String| {
            let slow = slow.clone();
            let started_reads = started_reads.clone();
            let finished_reads = finished_reads.clone();
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
                    && lower.contains("00000000000000000000000000000000000000a1")
                {
                    started_reads.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    finished_reads.fetch_add(1, Ordering::SeqCst);
                }
                let result = if body.contains("\"eth_chainId\"") {
                    "\"0x7a69\"".to_string()
                } else if body.contains("\"eth_blockNumber\"") {
                    "\"0x10\"".to_string()
                } else if body.contains("\"eth_getBlockByNumber\"")
                    || body.contains("\"eth_getBlockByHash\"")
                {
                    let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                    let tag = request["params"][0].as_str().unwrap();
                    let number = if body.contains("\"eth_getBlockByHash\"") {
                        let hash: alloy::primitives::B256 = tag.parse().unwrap();
                        u64::from_be_bytes(hash.as_slice()[24..].try_into().unwrap())
                    } else if matches!(tag, "latest" | "pending") {
                        16
                    } else {
                        u64::from_str_radix(tag.trim_start_matches("0x"), 16).unwrap()
                    };
                    let mut block =
                        alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                    block.header.inner.number = number;
                    block.header.inner.timestamp = 1_700_000_000;
                    block.header.hash = block_hash(number);
                    block.header.inner.parent_hash = if number == 0 {
                        alloy::primitives::B256::ZERO
                    } else {
                        block_hash(number - 1)
                    };
                    if body.contains("\"eth_getBlockByHash\"")
                        && tag != format!("{:#x}", block.header.hash)
                    {
                        "null".to_string()
                    } else {
                        serde_json::to_string(&block).unwrap()
                    }
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
    let started_reads = Arc::new(AtomicU64::new(0));
    let finished_reads = Arc::new(AtomicU64::new(0));
    let base = a_node(slow.clone(), started_reads.clone(), finished_reads.clone()).await;
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
    let portfolio_reply = tokio::spawn(async move { asking.get(portfolio).send().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while started_reads.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the portfolio must actually enter its delayed balance read");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        started_reads.load(Ordering::SeqCst) > finished_reads.load(Ordering::SeqCst),
        "the delayed balance read must still be in flight"
    );

    // The record is in memory: it is answered at once.
    let asked = Instant::now();
    let reply = http.get(&snapshot_url).send().await.unwrap();
    let took = asked.elapsed();
    assert!(reply.status().is_success());
    assert!(
        portfolio_reply
            .await
            .unwrap()
            .unwrap()
            .status()
            .is_success()
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        took < Duration::from_secs(2),
        "/api/snapshot took {took:?}: held up by one portfolio request's chain reads"
    );
}
