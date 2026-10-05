//! The API binds its port at once and serves each vault chain from the
//! moment it connects. One chain's node being down when the API starts
//! keeps none of the others from being answered for (the usage route, which
//! an author's tank withdrawal depends on, and the record), and the chain
//! that is down is answered for as unavailable (503) until it connects,
//! never as a chain this platform does not serve; meanwhile the API's own
//! health answers 503 and names it.
//!
//! Here base and hyperliquid answer and robinhood's node is not there.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::routing::post;

/// A node that answers as an EVM chain with `chain_id`, has code at every
/// address, and refuses every other question.
async fn a_node(chain_id: u64) -> SocketAddr {
    let app = Router::new().route(
        "/",
        post(move |body: String| async move {
            // `"id":<n>` as alloy writes it.
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

static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Start the API over base, hyperliquid and robinhood (robinhood's node
/// there or not) and say what its usage route answered for base, if it
/// answered within 25 seconds, then what it answered for robinhood, and
/// what its health route answered.
async fn answered(robinhood_up: bool) -> (Option<u16>, Option<u16>, Option<(u16, String)>) {
    // One API at a time: the port is found free and then let go, so two
    // started at once could be handed the same one.
    let _turn = ONE_AT_A_TIME.lock().await;
    let dir = std::env::temp_dir().join(format!(
        "shonin-api-one-chain-down-{}-{}",
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
    let hyperliquid = a_node(31338).await;
    // Nothing listens on port 1.
    let robinhood = if robinhood_up {
        format!("http://{}", a_node(31339).await)
    } else {
        "http://127.0.0.1:1".to_string()
    };
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
            rpc = "http://{hyperliquid}"
            dollar = "USDC"

            [chains."evm:31339"]
            label = "Robinhood"
            rpc = "{robinhood}"
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
    std::fs::write(
        deployments_dir.join("evm-31338.json"),
        deployments("evm:31338", 31338),
    )
    .unwrap();
    std::fs::write(
        deployments_dir.join("evm-31339.json"),
        deployments("evm:31339", 31339),
    )
    .unwrap();

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
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let usage = |chain: &str| {
        format!(
            "http://127.0.0.1:{port}/api/{chain}/usage/0x1111111111111111111111111111111111111111"
        )
    };
    let started = std::time::Instant::now();
    let mut base = None;
    while started.elapsed() < Duration::from_secs(25) {
        tokio::time::sleep(Duration::from_millis(500)).await;
        // Answered for once it has connected: the executor is not there, so
        // a connected chain's usage is unavailable from the executor (502).
        if let Ok(reply) = http.get(usage("base")).send().await
            && reply.status() != reqwest::StatusCode::SERVICE_UNAVAILABLE
        {
            base = Some(reply.status().as_u16());
            break;
        }
    }
    // Robinhood, once base has answered: given a moment to connect too when
    // its node is there.
    let mut robinhood = None;
    while started.elapsed() < Duration::from_secs(25) {
        robinhood = http
            .get(usage("robinhood"))
            .send()
            .await
            .ok()
            .map(|reply| reply.status().as_u16());
        if !robinhood_up || robinhood != Some(503) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let health = match http
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .await
    {
        Ok(reply) => Some((
            reply.status().as_u16(),
            reply.text().await.unwrap_or_default(),
        )),
        Err(_) => None,
    };
    let _ = std::fs::remove_dir_all(&dir);
    (base, robinhood, health)
}

/// The control: with every chain up, the API answers for each.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_every_chain_up_the_api_answers() {
    let (base, robinhood, _) = answered(true).await;
    assert_eq!(base, Some(502));
    assert_eq!(robinhood, Some(502));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_chain_out_of_reach_does_not_keep_the_api_from_the_others() {
    let (base, robinhood, health) = answered(false).await;
    assert_eq!(
        base,
        Some(502),
        "with robinhood's node down, the API had not answered for base after 25 s"
    );
    assert_eq!(
        robinhood,
        Some(503),
        "a chain not connected yet is answered for as unavailable"
    );
    // And the API's own health says so, rather than that all is well.
    let (status, said) = health.expect("the API answers for its health");
    assert_eq!(status, 503, "{said}");
    assert!(
        said.lines().next().unwrap_or_default().len() <= 60,
        "{said}"
    );
    assert!(said.contains("robinhood: not connected"), "{said}");
}
