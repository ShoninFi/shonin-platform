//! The public published-key route keeps each purpose's answer for a while so
//! the executor is not asked (and does not read the chain) once per public
//! request. Anyone may name a purpose (any bundle's, say), and every
//! well-formed one is answered with a signed key, so the kept answers are
//! bounded in number. That bound must not be a lever: a flood of made-up
//! purposes, each kept, must not leave a real purpose's key unkept, asked of
//! the executor again on every request for as long as the flood keeps the
//! table full. Nor can one purpose be kept many times over under spellings
//! of its own: only the canonical one is asked for.
//!
//! Here the API runs over a stand-in executor that answers every published
//! key and notes each path it is asked; the chains' nodes are not there,
//! which holds up none of the root key routes.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::routing::get;

const KEY: &str =
    r#"{"chain_id":31337,"purpose":"platform:dummy","public_key":"0x00","signature":"0x00"}"#;

/// An executor whose root key routes answer every published key, noting
/// each path asked.
async fn an_executor() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let noted = asked.clone();
    let app = Router::new().fallback(get(move |uri: axum::http::Uri| {
        let noted = noted.clone();
        async move {
            noted.lock().unwrap().push(uri.path().to_string());
            (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                KEY,
            )
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (address, asked)
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

/// How many times the executor was asked for `purpose`'s published key.
fn times_asked(asked: &Mutex<Vec<String>>, purpose: &str) -> usize {
    let path = format!("/rootkey/published/{purpose}");
    asked.lock().unwrap().iter().filter(|p| **p == path).count()
}

#[tokio::test]
async fn a_flood_of_purposes_cannot_keep_a_published_key_from_being_kept() {
    let dir = std::env::temp_dir().join(format!(
        "shonin-api-published-keys-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let deployments_dir = dir.join("deployments");
    std::fs::create_dir_all(&deployments_dir).unwrap();
    std::fs::create_dir_all(dir.join("store")).unwrap();
    // Nothing listens on port 1: the chains never connect, and the root key
    // routes need none of them.
    std::fs::write(
        dir.join("chains.toml"),
        r#"
        [chains."evm:31337"]
        label = "Base (local)"
        rpc = "http://127.0.0.1:1"
        dollar = "USDC"

        [chains."evm:31338"]
        label = "Hyperliquid (local)"
        rpc = "http://127.0.0.1:1"
        dollar = "USDC"

        [chains."evm:31339"]
        label = "Robinhood"
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
        "#,
    )
    .unwrap();
    for (key, id) in [
        ("evm:31337", 31337),
        ("evm:31338", 31338),
        ("evm:31339", 31339),
    ] {
        std::fs::write(
            deployments_dir.join(format!("evm-{id}.json")),
            deployments(key, id),
        )
        .unwrap();
    }

    let (executor, asked) = an_executor().await;
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let _api = Killed(
        std::process::Command::new(env!("CARGO_BIN_EXE_api"))
            .args(["--network", "local", "--chains"])
            .arg(dir.join("chains.toml"))
            .arg("--deployments-dir")
            .arg(&deployments_dir)
            .arg("--store")
            .arg(dir.join("store"))
            .args([
                "--builder-url",
                "http://127.0.0.1:9",
                "--executor-url",
                &format!("http://{executor}"),
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
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let published =
        |purpose: &str| format!("http://127.0.0.1:{port}/api/rootkey/published/{purpose}");

    // Up: the first purpose's key comes through the executor.
    let started = std::time::Instant::now();
    loop {
        if let Ok(reply) = http.get(published("builder")).send().await
            && reply.status() == reqwest::StatusCode::OK
        {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "the API did not come up"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    // Asked again at once, it is kept: the executor is not asked twice.
    assert_eq!(
        http.get(published("builder"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(times_asked(&asked, "builder"), 1);

    // Another spelling of a purpose is refused, never asked of the executor.
    let vault = "0x1111111111111111111111111111111111111111";
    for spelling in [
        format!("venue:+998:{vault}:hyperliquid"),
        format!("venue:0998:{vault}:hyperliquid"),
    ] {
        let reply = http.get(published(&spelling)).send().await.unwrap();
        assert_eq!(
            reply.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{spelling}"
        );
        assert_eq!(times_asked(&asked, &spelling), 0);
    }

    // Anyone names a thousand purposes of their own, each answered and
    // kept.
    for each in 0..1024u32 {
        let reply = http
            .get(published(&format!("bundle:0x{each:064x}")))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), reqwest::StatusCode::OK, "bundle {each}");
    }

    // A real purpose asked for twice in a row, while the flood's answers
    // are still fresh: kept after the first, so the executor is asked once.
    for _ in 0..2 {
        let reply = http.get(published("platform:dummy")).send().await.unwrap();
        assert_eq!(reply.status(), reqwest::StatusCode::OK);
    }
    assert_eq!(
        times_asked(&asked, "platform:dummy"),
        1,
        "a flood of made-up purposes kept a real purpose's key from being kept: the executor was asked for it on every request"
    );
}
