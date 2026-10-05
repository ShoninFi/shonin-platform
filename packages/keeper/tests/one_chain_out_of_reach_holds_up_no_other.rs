//! The keeper is started while two of its vault chains' nodes are out of
//! reach. Each chain is connected to on its own, and its rounds start as soon
//! as it is: the chain that answers is kept at once, while the others are
//! tried again. Here Base answers, Hyperliquid and Robinhood refuse
//! connections; within 20 seconds the keeper has read Base's factory for its
//! vaults.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use alloy::sol_types::SolCall;
use axum::Json;
use axum::extract::State;
use axum::routing::post;
use chain::bindings::factory::IVaultFactory;
use chain::{Deployments, Roles};
use serde_json::{Value, json};

fn accounts() -> Vec<String> {
    (1u8..=10)
        .map(|n| format!("{:?}", Address::repeat_byte(n)))
        .collect()
}

fn answer(reads: &AtomicUsize, asked: &Value) -> Value {
    let id = asked["id"].clone();
    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
    match asked["method"].as_str().unwrap_or_default() {
        "eth_chainId" => ok(json!("0x7a69")),
        "eth_accounts" => ok(json!(accounts())),
        "eth_blockNumber" => ok(json!("0x10")),
        "eth_call" => {
            let data = asked["params"][0]
                .get("input")
                .or_else(|| asked["params"][0].get("data"))
                .and_then(Value::as_str)
                .unwrap_or("0x");
            let data = alloy::hex::decode(data).unwrap_or_default();
            // The keeper's check that the node simulates (code run with no
            // contract to call, or through a state override): a node that
            // does answers it, here with nothing come home and nothing left.
            let params = &asked["params"];
            if params[0].get("to").is_none_or(Value::is_null)
                || params.get(2).is_some_and(|overrides| !overrides.is_null())
            {
                return ok(json!(alloy::hex::encode_prefixed([0u8; 96])));
            }
            if data.len() >= 4 && data[..4] == IVaultFactory::vaultsCall::SELECTOR {
                reads.fetch_add(1, Ordering::SeqCst);
                // No vaults: an empty address[].
                let mut out = vec![0u8; 64];
                out[31] = 0x20;
                return ok(json!(alloy::hex::encode_prefixed(out)));
            }
            // `previous()`: no earlier factory.
            ok(json!(alloy::hex::encode_prefixed([0u8; 32])))
        }
        other => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("not here: {other}")}})
        }
    }
}

async fn a_node_that_answers(reads: Arc<AtomicUsize>) -> String {
    let app = axum::Router::new()
        .route(
            "/",
            post(
                |State(reads): State<Arc<AtomicUsize>>, Json(body): Json<Value>| async move {
                    Json(match &body {
                        Value::Array(batch) => {
                            Value::Array(batch.iter().map(|one| answer(&reads, one)).collect())
                        }
                        one => answer(&reads, one),
                    })
                },
            ),
        )
        .with_state(reads);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    url
}

fn deployments() -> Deployments {
    let nobody = Address::repeat_byte(0x55);
    Deployments {
        chain_key: "evm:31337".to_string(),
        chain_id: 31337,
        rpc_url: String::new(),
        deployed_block: 1,
        usd: nobody,
        venue: nobody,
        attestation_verifier: nobody,
        attestation: nobody,
        publishing: nobody,
        prices: nobody,
        strategies: nobody,
        reviews: nobody,
        profiles: nobody,
        factory: Address::repeat_byte(0x66),
        station_factory: nobody,
        driver: nobody,
        bridge: nobody,
        closer: Address::ZERO,
        roles: Roles {
            admin: nobody,
            platform: nobody,
            attestor: nobody,
            builder: nobody,
            reviewer: nobody,
            executor: nobody,
            keeper: Address::repeat_byte(5),
            operator: nobody,
            author: nobody,
            viewer: nobody,
        },
    }
}

/// Start the keeper against Base (answering) and the other two chains
/// (refusing), optionally told to serve only `only`. How many times it read
/// Base's vaults within 20 seconds, and whether it had exited.
async fn reads_within_20s(only: Option<&str>) -> (usize, Option<std::process::ExitStatus>) {
    let reads = Arc::new(AtomicUsize::new(0));
    let up = a_node_that_answers(reads.clone()).await;
    let down = "http://127.0.0.1:1";

    let dir = std::env::temp_dir().join(format!(
        "keeper-one-chain-down-{}-{}",
        std::process::id(),
        only.unwrap_or("all")
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let chains = dir.join("chains.toml");
    let mut file = std::fs::File::create(&chains).unwrap();
    write!(
        file,
        r#"
[chains."evm:31337"]
label = "Base (up)"
rpc = "{up}"
dollar = "USDC"

[chains."evm:31338"]
label = "Hyperliquid (down)"
rpc = "{down}"
dollar = "USDC"

[chains."evm:31339"]
label = "Robinhood (down)"
rpc = "{down}"
dollar = "USDG"

[chains."solana:localnet"]
label = "Solana (not served)"
rpc = "{down}"
dollar = "USDC"

[networks.local]
base = "evm:31337"
hyperliquid = "evm:31338"
robinhood = "evm:31339"
solana = "solana:localnet"
"#
    )
    .unwrap();
    drop(file);

    let mut keeper = Command::new(env!("CARGO_BIN_EXE_keeper"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(
            "SHONIN_DEPLOYMENTS_JSON",
            serde_json::to_string(&vec![deployments()]).unwrap(),
        )
        .args(["--network", "local"])
        .arg("--chains")
        .arg(&chains)
        .arg("--deployments-dir")
        .arg(&dir)
        .arg("--store")
        .arg(&dir)
        .args(["--api-url", down, "--executor-url", down])
        .args(
            only.map(|chain| vec!["--vault-chain", chain])
                .unwrap_or_default(),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(20) && reads.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let exited = keeper.try_wait().unwrap();
    let _ = keeper.kill();
    let _ = keeper.wait();
    let _ = std::fs::remove_dir_all(&dir);
    (reads.load(Ordering::SeqCst), exited)
}

/// Told to serve Base alone, the same keeper reads its vaults at once: the
/// stand-in node and the arguments are sound.
#[tokio::test(flavor = "multi_thread")]
async fn a_keeper_serving_only_the_chain_that_answers_reads_it() {
    let (reads, _) = reads_within_20s(Some("base")).await;
    assert!(reads > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chain_out_of_reach_at_start_holds_up_no_other_chain() {
    let (reads, exited) = reads_within_20s(None).await;
    assert!(
        reads > 0,
        "20 s after start the keeper had not read Base's vaults (exited: {exited:?}): \
         one chain out of reach held up the chain that answers"
    );
}
