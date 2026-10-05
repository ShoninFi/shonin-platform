//! The executor's health, on its status port, answers 503 while a vault chain
//! it serves isn't connected, naming it, and 200 once every one is: a
//! dashboard watching it sees an executor that can reach none of its vaults
//! as down, as it sees the API and the indexer.

mod status_port;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::post;

/// A node that answers as an EVM chain with `chain_id` and refuses every
/// other question.
async fn a_node(chain_id: u64) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |body: String| async move {
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
            let result = if body.contains("\"eth_chainId\"") {
                Some(format!("\"0x{chain_id:x}\""))
            } else if body.contains("\"eth_getCode\"") {
                Some("\"0x01\"".to_string())
            } else if body.contains("\"eth_blockNumber\"") {
                Some("\"0x10\"".to_string())
            } else if body.contains("\"eth_accounts\"") {
                Some("[]".to_string())
            } else {
                None
            };
            let reply = match result {
                Some(result) => format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#),
                None => format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"error":{{"code":3,"message":"execution reverted"}}}}"#
                ),
            };
            ([(axum::http::header::CONTENT_TYPE, "application/json")], reply)
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

fn deployments(chain_key: &str, chain_id: u64) -> String {
    let one = "0x0000000000000000000000000000000000000001";
    let mut fields = vec![
        format!(r#""chain_key":"{chain_key}""#),
        format!(r#""chain_id":{chain_id}"#),
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

/// An executor on the local network, base answered by a node of its own and
/// every other chain out of reach; `only_base` serves base alone.
async fn an_executor(only_base: bool) -> (status_port::Started, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "shonin-executor-health-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let deployments_dir = dir.join("deployments");
    std::fs::create_dir_all(&deployments_dir).unwrap();
    std::fs::create_dir_all(dir.join("store")).unwrap();
    let base = a_node(31337).await;
    std::fs::write(
        dir.join("chains.toml"),
        format!(
            r#"
            [chains."evm:31337"]
            label = "Base (local)"
            rpc = "http://{base}"
            dollar = "USDC"

            [chains."evm:31338"]
            label = "Hyperliquid (local)"
            rpc = "http://127.0.0.1:1"
            dollar = "USDC"

            [chains."evm:31339"]
            label = "Robinhood (local)"
            rpc = "http://127.0.0.1:1"
            dollar = "USDG"

            [chains."solana:localnet"]
            label = "Solana (local)"
            rpc = "http://127.0.0.1:1"
            dollar = "USDC"

            [networks.local]
            base = "evm:31337"
            hyperliquid = "evm:31338"
            robinhood = "evm:31339"
            solana = "solana:localnet"
            "#
        ),
    )
    .unwrap();
    std::fs::write(
        deployments_dir.join("evm-31337.json"),
        deployments("evm:31337", 31337),
    )
    .unwrap();

    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_executor"));
    command.args(["--network", "local"]);
    if only_base {
        command.args(["--vault-chain", "base"]);
    }
    command
        .arg("--chains")
        .arg(dir.join("chains.toml"))
        .arg("--deployments-dir")
        .arg(&deployments_dir)
        .arg("--store")
        .arg(dir.join("store"))
        .args(["--api-url", "http://127.0.0.1:9"])
        .env_remove("SHONIN_DEPLOYMENTS_JSON")
        .env_remove("SHONIN_VAULT_CHAIN")
        .env_remove("SHONIN_NETWORK")
        .env_remove("SHONIN_CHAINS")
        .env_remove("SHONIN_DEPLOYMENTS_DIR");
    (status_port::start(&mut command).await, dir)
}

/// Ask the executor's health until `done` holds of its answer, or 30 s pass;
/// the last answer.
async fn health_until(port: u16, done: impl Fn(u16, &str) -> bool) -> (u16, String) {
    let began = Instant::now();
    let mut last = (0, String::new());
    while began.elapsed() < Duration::from_secs(30) {
        if let Ok(reply) = reqwest::get(format!("http://127.0.0.1:{port}/health")).await {
            let status = reply.status().as_u16();
            last = (status, reply.text().await.unwrap_or_default());
            if done(last.0, &last.1) {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    last
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_executor_is_unavailable_while_a_chain_it_serves_is_not_connected() {
    let (mut started, dir) = an_executor(false).await;
    let (status, said) = health_until(started.port, |_, said| {
        said.lines().any(|line| line == "base: connected")
    })
    .await;
    let _ = started.child.kill();
    let _ = started.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        said.lines().any(|line| line == "base: connected"),
        "base never connected: {said}"
    );
    assert_eq!(
        status, 503,
        "chains it serves are not connected, and its health answered {status}: {said}"
    );
    let first = said.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("executor: ") && first.ends_with("chains not connected"),
        "{said}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_executor_is_up_once_every_chain_it_serves_has_connected() {
    let (mut started, dir) = an_executor(true).await;
    let (status, said) = health_until(started.port, |status, _| status == 200).await;
    let _ = started.child.kill();
    let _ = started.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(status, 200, "{said}");
    assert_eq!(said, "executor\nbase: connected");
}
