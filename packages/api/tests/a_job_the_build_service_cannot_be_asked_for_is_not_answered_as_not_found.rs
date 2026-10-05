//! `/api/jobs/{id}` asks the build service for the job. A build service that
//! can't be reached is answered 502, and one that answers 503 (restarting, or
//! the job's chain not connected yet) is answered 503: the job is not gone,
//! and only the build service saying there is no such job is answered 404.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};

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

struct Killed(std::process::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A build service that answers 503 for job 1, 404 for job 2 and 500 for
/// job 3.
async fn a_build_service() -> SocketAddr {
    let app = Router::new().route(
        "/jobs/{id}",
        get(
            |axum::extract::Path(id): axum::extract::Path<String>| async move {
                match id.as_str() {
                    "1" => axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "2" => axum::http::StatusCode::NOT_FOUND,
                    _ => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

/// An API on one local chain, asking the build service at `builder`; its
/// port and the directory it runs from.
async fn an_api(builder: &str) -> (Killed, u16, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "shonin-api-job-builder-{}-{}",
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

    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let api = Killed(
        std::process::Command::new(env!("CARGO_BIN_EXE_api"))
            .args(["--network", "local", "--chains"])
            .arg(dir.join("chains.toml"))
            .arg("--deployments-dir")
            .arg(&deployments_dir)
            .arg("--store")
            .arg(dir.join("store"))
            .args([
                "--builder-url",
                builder,
                "--executor-url",
                "http://127.0.0.1:1",
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
    (api, port, dir)
}

/// What the API answers for job `id`, once it answers at all.
async fn asked_for(port: u16, id: u32) -> u16 {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(25) {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(reply) = http
            .get(format!("http://127.0.0.1:{port}/api/jobs/{id}"))
            .send()
            .await
        {
            return reply.status().as_u16();
        }
    }
    panic!("the API answers for a job within 25 s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_the_build_service_cannot_be_asked_for_is_not_answered_as_not_found() {
    // Nothing listens on port 1: the build service can't be reached.
    let (api, port, dir) = an_api("http://127.0.0.1:1").await;
    let status = asked_for(port, 1).await;
    drop(api);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        status, 502,
        "a job the build service can't be reached for is answered as a bad gateway"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_service_that_cannot_answer_for_now_is_passed_on_and_only_its_not_found_is_404() {
    let builder = a_build_service().await;
    let (api, port, dir) = an_api(&format!("http://{builder}")).await;
    let unavailable = asked_for(port, 1).await;
    let missing = asked_for(port, 2).await;
    let failed = asked_for(port, 3).await;
    drop(api);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(unavailable, 503, "the build service's 503 is passed on");
    assert_eq!(missing, 404, "a job the build service has not is not found");
    assert_eq!(
        failed, 502,
        "any other failure of the build service is a bad gateway"
    );
}
