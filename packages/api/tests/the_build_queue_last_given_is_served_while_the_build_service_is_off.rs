//! The API keeps the build service's last answer for its queue
//! (`jobs-cache.json`, in its own state beside the store), and while the
//! build service is off (it runs only while there is work) it serves that
//! queue, after a restart as before one: `/api/jobs` lists it and
//! `/api/jobs/{id}` answers each job in it, never a bad gateway.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::routing::{get, post};
use shared::model::registry::{Job, JobState};

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

fn a_job(id: &str) -> Job {
    Job {
        id: id.to_string(),
        chain: shared::Chain::Base,
        strategy_id: None,
        title: "funding basis".to_string(),
        author: "0x2222222222222222222222222222222222222222".to_string(),
        state: JobState::Queued { position: 1 },
        submitted: time::PrimitiveDateTime::new(
            time::Date::from_ordinal_date(2026, 281).unwrap(),
            time::Time::MIDNIGHT,
        ),
        duration_seconds: 0,
        source_hash: format!("sha256:{}", "0".repeat(64)),
        sealed: false,
        bundle_hash: None,
        review: None,
        strategy_slug: None,
    }
}

/// A build service listing `jobs`.
async fn a_build_service(jobs: Vec<Job>) -> SocketAddr {
    let body = serde_json::to_string(&jobs).unwrap();
    let app = Router::new().route(
        "/jobs",
        get(move || {
            let body = body.clone();
            async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

/// An API on one local chain in `dir`, asking the build service at
/// `builder`, and its port.
async fn an_api(builder: &str, dir: &std::path::Path) -> (Killed, u16) {
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
    (api, port)
}

/// What the API answers at `path` once it answers `wanted`, or last, within
/// 25 s.
async fn asked(port: u16, path: &str, wanted: impl Fn(u16, &str) -> bool) -> (u16, String) {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let started = std::time::Instant::now();
    let mut last = (0, String::new());
    while started.elapsed() < Duration::from_secs(25) {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(reply) = http
            .get(format!("http://127.0.0.1:{port}{path}"))
            .send()
            .await
        {
            let status = reply.status().as_u16();
            let body = reply.text().await.unwrap_or_default();
            if wanted(status, &body) {
                return (status, body);
            }
            last = (status, body);
        }
    }
    last
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_build_queue_last_given_is_served_while_the_build_service_is_off() {
    let dir = std::env::temp_dir().join(format!(
        "shonin-api-jobs-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    // The build service up: its queue is listed, and kept.
    let builder = a_build_service(vec![a_job("8801")]).await;
    let (api, port) = an_api(&format!("http://{builder}"), &dir).await;
    let (status, listed) = asked(port, "/api/jobs", |_, body| body.contains("8801")).await;
    let cache = dir.join("api-state").join("jobs-cache.json");
    let started = std::time::Instant::now();
    while !cache.exists() && started.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    drop(api);
    assert_eq!(status, 200);
    assert!(listed.contains("8801"), "{listed}");
    assert!(cache.exists(), "the queue is kept beside the store");

    // Restarted with the build service off (nothing listens on port 1).
    let (api, port) = an_api("http://127.0.0.1:1", &dir).await;
    let (status, listed) = asked(port, "/api/jobs", |_, body| body.contains("8801")).await;
    let (one, job) = asked(port, "/api/jobs/8801", |status, _| status == 200).await;
    let (other, _) = asked(port, "/api/jobs/8802", |status, _| status != 0).await;
    drop(api);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(status, 200);
    assert!(
        listed.contains("8801"),
        "the queue last given is listed: {listed}"
    );
    assert_eq!(one, 200, "a job in the queue last given is answered: {job}");
    assert!(job.contains("funding basis"), "{job}");
    assert_eq!(
        other, 503,
        "a job never listed is unavailable, not gone and not a bad gateway"
    );
}
