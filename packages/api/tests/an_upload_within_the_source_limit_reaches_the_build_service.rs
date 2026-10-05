//! An upload is JSON carrying the project's archive as base64 (4/3 its
//! size), and the build service takes up to 2 MiB of source. The API
//! takes a body large enough for that, so a project within the limit reaches
//! the build service rather than being turned away with a bare 413 that never
//! says what the limit is.
//!
//! A build service stands in that takes any upload. A small upload is passed
//! through, and so is one whose archive is 1.7 MB.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::post;

async fn a_node() -> SocketAddr {
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
                "\"0x7a69\"".to_string()
            } else if body.contains("\"eth_getCode\"") {
                "\"0x01\"".to_string()
            } else if body.contains("\"eth_blockNumber\"") {
                "\"0x10\"".to_string()
            } else {
                "[]".to_string()
            };
            (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result}}}"#),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

/// A build service that takes any upload, however large.
async fn a_builder() -> String {
    let app = Router::new()
        .route(
            "/{chain}/uploads",
            post(|_body: axum::body::Bytes| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    r#"{"upload":"0x01","job_id":"1"}"#,
                )
            }),
        )
        .layer(axum::extract::DefaultBodyLimit::disable());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_within_the_source_limit_reaches_the_build_service() {
    let dir = std::env::temp_dir().join(format!(
        "shonin-api-upload-limit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let deployments_dir = dir.join("deployments");
    std::fs::create_dir_all(&deployments_dir).unwrap();
    std::fs::create_dir_all(dir.join("store")).unwrap();
    let base = a_node().await;
    let builder = a_builder().await;
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
                &builder,
                "--executor-url",
                "http://127.0.0.1:9",
            ])
            .args(["--port", &port.to_string()])
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
    let url = format!("http://127.0.0.1:{port}/api/base/uploads");
    let upload = |bytes: usize| {
        let archive = vec![b'a'; bytes];
        serde_json::json!({
            "author": "0x00000000000000000000000000000000000000a1",
            "archive_base64": shared::base64::encode(&archive),
            "sealed": false,
        })
    };

    // A small upload is carried through once base is served.
    let started = Instant::now();
    loop {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "base never served"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        if let Ok(reply) = http.post(&url).json(&upload(1_000)).send().await
            && reply.status() == 200
        {
            break;
        }
    }
    // 1.7 MB: under the build service's 2 MiB source limit, and over 2 MiB
    // once carried as base64.
    let reply = http
        .post(&url)
        .json(&upload(1_700_000))
        .send()
        .await
        .unwrap();
    let status = reply.status().as_u16();
    let body = reply.text().await.unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        status, 200,
        "an archive under the source limit was turned away by the API: {status} {body}"
    );
}
