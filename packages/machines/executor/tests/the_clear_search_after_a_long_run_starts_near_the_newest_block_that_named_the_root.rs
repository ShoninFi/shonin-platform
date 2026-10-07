//! When a recorded root's record reads empty, the executor searches the
//! chain's log for that signer's clear before it decides anything. A block
//! whose record names the held root rules out any clear at or before it (a
//! cleared signer is retired for good), so every look that finds the record
//! naming it moves the search's start on to a few dozen blocks below that
//! block. The first empty read after a long run then searches only the last
//! few dozen blocks, not every block since the root was made: a month of
//! Base's two-second blocks is 1.3 million, which Base's own endpoint hands
//! out 500 at a time, and a look that made thousands of log requests would
//! fail the moment any of them did.
//!
//! Here the stand-in node records the root the executor makes, lets it be
//! served, then jumps its head 200,000 blocks on (as a month passes), answers
//! one more look with the record intact at the new head, and then answers
//! the record empty, with no clear logged. It refuses any log request wider
//! than 500 blocks, as Base's endpoint does. The executor reaches its
//! decision (the root kept, unserved) with a handful of log requests.

mod status_port;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, U256};
use alloy::sol_types::{SolCall, SolValue};
use axum::Json;
use axum::extract::State;
use axum::routing::post;
use chain::bindings::attestation::IAttestationRegistry;
use serde_json::{Value, json};

const FIRST_HEAD: u64 = 100;
const GENESIS_TIME: u64 = 1_700_000_000;
const TTL: u64 = 72 * 3600;
/// How far the head jumps while the root is served: a few days of Base's
/// blocks, enough to tell a search from the making block from one near the
/// head.
const JUMP: u64 = 200_000;
/// The widest log range the node answers, as Base's own endpoint.
const WIDEST_LOG_RANGE: u64 = 500;
/// How many log requests a look may reasonably make: the last few dozen
/// blocks, in one window, with room for a retry or two.
const REASONABLE_LOG_REQUESTS: usize = 8;

#[derive(Default)]
struct Node {
    head: u64,
    /// The signer the record names, and the block it was set at.
    set: Option<(Address, u64)>,
    /// From this block on the record reads empty (a fork that lost the
    /// record's transaction); no clear is ever logged.
    empty_from: Option<u64>,
    /// Every block the record was read at.
    record_reads: Vec<u64>,
    /// Every log request: its range.
    logs_asked: Vec<(u64, u64)>,
}

impl Node {
    fn time(block: u64) -> u64 {
        GENESIS_TIME + 2 * block
    }

    fn signer_at(&self, block: u64) -> Option<(Address, u64)> {
        if self.empty_from.is_some_and(|from| block >= from) {
            return None;
        }
        self.set.filter(|(_, from)| block >= *from)
    }
}

fn word(value: u64) -> String {
    alloy::hex::encode_prefixed(U256::from(value).to_be_bytes::<32>())
}

fn truth(value: bool) -> String {
    word(u64::from(value))
}

fn record(signer: Address, time: u64) -> String {
    let encoded = (
        signer,
        time,
        Address::repeat_byte(1),
        time,
        B256::repeat_byte(0xab),
        B256::repeat_byte(0xcd),
    )
        .abi_encode_params();
    alloy::hex::encode_prefixed(encoded)
}

fn block(number: u64) -> Value {
    let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
    block.header.inner.number = number;
    block.header.inner.timestamp = Node::time(number);
    serde_json::to_value(block).unwrap()
}

fn quantity(value: &Value) -> Option<u64> {
    u64::from_str_radix(value.as_str()?.strip_prefix("0x")?, 16).ok()
}

fn answer(node: &Mutex<Node>, asked: &Value) -> Value {
    let id = asked["id"].clone();
    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
    let refused = |message: &str| json!({"jsonrpc": "2.0", "id": id, "error": {"code": 3, "message": message}});
    let mut node = node.lock().unwrap();
    if node.head == 0 {
        node.head = FIRST_HEAD;
    }
    let params = &asked["params"];
    match asked["method"].as_str().unwrap_or_default() {
        "eth_chainId" => ok(json!("0x7a69")),
        "eth_accounts" => ok(json!([])),
        "eth_getCode" => ok(json!("0x01")),
        "eth_getLogs" => {
            let filter = &params[0];
            let from = quantity(&filter["fromBlock"]).unwrap_or(0);
            let to = quantity(&filter["toBlock"]).unwrap_or(node.head);
            node.logs_asked.push((from, to));
            if to.saturating_sub(from) >= WIDEST_LOG_RANGE {
                return json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": "block range is too wide: at most 500 blocks"}});
            }
            // No clear of any signer, ever.
            ok(json!([]))
        }
        "eth_blockNumber" => ok(json!(format!("0x{:x}", node.head))),
        "eth_getBlockByNumber" => match quantity(&params[0]) {
            None if matches!(params[0].as_str(), Some("safe" | "finalized")) => {
                ok(block(node.head))
            }
            Some(number) if number <= node.head => ok(block(number)),
            Some(_) => ok(Value::Null),
            None => {
                node.head += 1;
                ok(block(node.head))
            }
        },
        "eth_call" => {
            let at = match params.get(1) {
                Some(tag) if tag.is_string() => match quantity(tag) {
                    Some(number) => number,
                    None => node.head,
                },
                _ => node.head,
            };
            if at > node.head {
                return refused("header not found");
            }
            let data = params[0]
                .get("input")
                .or_else(|| params[0].get("data"))
                .and_then(Value::as_str)
                .unwrap_or("0x");
            let data = alloy::hex::decode(data).unwrap_or_default();
            if data.len() < 4 {
                return refused("execution reverted");
            }
            let selector: [u8; 4] = data[..4].try_into().unwrap();
            let recorded = node.signer_at(at);
            match selector {
                IAttestationRegistry::rootKeyCall::SELECTOR => {
                    node.record_reads.push(at);
                    match recorded {
                        None => ok(json!(record(Address::ZERO, 0))),
                        Some((signer, from)) => ok(json!(record(signer, Node::time(from)))),
                    }
                }
                IAttestationRegistry::isRootKeyLiveCall::SELECTOR => {
                    ok(json!(truth(recorded.is_some())))
                }
                IAttestationRegistry::isImageAllowedCall::SELECTOR
                | IAttestationRegistry::isProjectAllowedCall::SELECTOR
                | IAttestationRegistry::isVerifierAllowedCall::SELECTOR => ok(json!(truth(true))),
                IAttestationRegistry::blessingTtlCall::SELECTOR => ok(json!(word(TTL))),
                _ => refused("execution reverted"),
            }
        }
        other => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("not here: {other}")}})
        }
    }
}

async fn a_node(node: Arc<Mutex<Node>>) -> SocketAddr {
    let app = axum::Router::new()
        .route(
            "/",
            post(
                |State(node): State<Arc<Mutex<Node>>>, Json(body): Json<Value>| async move {
                    Json(match &body {
                        Value::Array(batch) => {
                            Value::Array(batch.iter().map(|one| answer(&node, one)).collect())
                        }
                        one => answer(&node, one),
                    })
                },
            ),
        )
        .with_state(node);
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

async fn an_executor(node: SocketAddr) -> (status_port::Started, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "shonin-executor-root-search-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let deployments_dir = dir.join("deployments");
    std::fs::create_dir_all(&deployments_dir).unwrap();
    std::fs::create_dir_all(dir.join("store")).unwrap();
    std::fs::write(
        dir.join("chains.toml"),
        format!(
            r#"
            [chains."evm:31337"]
            label = "Base (local)"
            rpc = "http://{node}"
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
            root = "base"
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
    command
        .args(["--network", "local", "--vault-chain", "base"])
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
        .env_remove("SHONIN_DEPLOYMENTS_DIR")
        .env_remove("SHONIN_ROOT_PEERS");
    (status_port::start(&mut command).await, dir)
}

#[derive(Debug, serde::Deserialize)]
struct RootStatus {
    holds_root: bool,
    signer: Option<Address>,
    serving: bool,
}

async fn root_status(port: u16) -> Option<RootStatus> {
    reqwest::get(format!("http://127.0.0.1:{port}/rootkey"))
        .await
        .ok()?
        .json()
        .await
        .ok()
}

async fn status_until(
    port: u16,
    within: Duration,
    done: impl Fn(&RootStatus) -> bool,
) -> Option<RootStatus> {
    let began = Instant::now();
    let mut last = None;
    while began.elapsed() < within {
        if let Some(status) = root_status(port).await {
            let found = done(&status);
            last = Some(status);
            if found {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    last
}

async fn node_until(node: &Mutex<Node>, within: Duration, done: impl Fn(&Node) -> bool) -> bool {
    let began = Instant::now();
    while began.elapsed() < within {
        if done(&node.lock().unwrap()) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
}

async fn scenario(node: &Arc<Mutex<Node>>, port: u16) -> Result<(), String> {
    // The record is empty: the executor makes a root.
    let made = status_until(port, Duration::from_secs(90), |status| status.holds_root)
        .await
        .filter(|status| status.holds_root)
        .ok_or("the executor never made a root")?;
    let first = made.signer.ok_or("a root held with no signer")?;

    // The chain records it from the next block: served.
    {
        let mut node = node.lock().unwrap();
        let from = node.head + 1;
        node.set = Some((first, from));
    }
    status_until(port, Duration::from_secs(60), |status| status.serving)
        .await
        .filter(|status| status.serving && status.signer == Some(first))
        .ok_or("the executor never served the recorded root")?;

    // A long run: the head jumps on, and the executor's next look finds the
    // record still naming its root at the new head.
    let jumped = {
        let mut node = node.lock().unwrap();
        node.head += JUMP;
        node.head
    };
    if !node_until(node, Duration::from_secs(150), |node| {
        node.record_reads.iter().any(|at| *at >= jumped)
    })
    .await
    {
        return Err("the executor never looked at the record after the head jumped".into());
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let served = root_status(port)
        .await
        .ok_or("the status port stopped answering")?;
    if !served.serving || served.signer != Some(first) {
        return Err(format!(
            "the root is no longer served after the head jumped: {served:?}"
        ));
    }

    // Then the record reads empty, with no clear logged: a reorganisation,
    // or a node on a fork. The executor searches the log and keeps the root,
    // unserved.
    let logs_before = node.lock().unwrap().logs_asked.len();
    {
        let mut node = node.lock().unwrap();
        node.empty_from = Some(node.head + 1);
    }
    let decided = status_until(port, Duration::from_secs(150), |status| !status.serving)
        .await
        .ok_or("the status port stopped answering")?;
    if !decided.holds_root || decided.signer != Some(first) {
        return Err(format!(
            "the root was not kept through the empty read: {decided:?}"
        ));
    }
    if decided.serving {
        return Err("the executor never acted on the empty read".into());
    }
    let asked: Vec<(u64, u64)> = node.lock().unwrap().logs_asked[logs_before..].to_vec();
    let widest = asked
        .iter()
        .map(|(from, to)| to - from + 1)
        .max()
        .unwrap_or(0);
    let earliest = asked.iter().map(|(from, _)| *from).min().unwrap_or(0);
    if asked.len() > REASONABLE_LOG_REQUESTS {
        return Err(format!(
            "deciding on one empty read took {} log requests (the earliest from block {earliest}, \
             the widest {widest} blocks, the head near {jumped}): the search for the clear started \
             at the block the root was made at, not near the newest block that showed the record \
             naming it",
            asked.len()
        ));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_clear_search_after_a_long_run_starts_near_the_newest_block_that_named_the_root() {
    let node = Arc::new(Mutex::new(Node::default()));
    let address = a_node(node.clone()).await;
    let (mut started, dir) = an_executor(address).await;
    let outcome = scenario(&node, started.port).await;
    let said = started.said.lock().unwrap().clone();
    let _ = started.child.kill();
    let _ = started.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(why) = outcome {
        panic!("{why}\n\nthe executor said:\n{said}");
    }
}
