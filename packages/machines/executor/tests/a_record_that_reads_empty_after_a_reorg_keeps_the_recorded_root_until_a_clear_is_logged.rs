//! The executor holds the only copy of the root key. A chain's head can be
//! reorganised a few blocks deep, and a public RPC name can front nodes on
//! different forks for a moment: a read then lands on a block, newer by
//! number than the one that showed the record, in which the transaction that
//! set the record is not yet (or no longer) included. Nobody cleared
//! anything: the chain never logged `RootKeyCleared` for that signer, and a
//! few blocks later the record names the same root again.
//!
//! An admin's clear always leaves that log. So the only copy of a recorded
//! root is never let go on an empty read alone: without the clear in the
//! chain's log the root is kept (and not served) until the record names it
//! again, or the log shows the clear. Letting go on the empty read alone
//! would wipe the root, make another the chain will never record, and leave
//! the chain naming a root nobody holds, until the admin cleared it and
//! every sealed secret was sealed again.
//!
//! Here the canonical chain is one stand-in node that answers each read as of
//! the block it is pinned to, its head moving on one block at each read. It
//! records nothing until the executor has made a root, then records that root
//! from the next block on; once the executor serves it, the node's next two
//! reads of the record, at later blocks, find it empty, as a node on a fork
//! that lost the record's transaction answers, and from then on it answers
//! truthfully again. It never logs a clear. The executor's `/rootkey` must
//! still name the same root, held and served, and it must never have taken
//! the empty reads for an admin's clear.

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

/// The block the node's head starts at, and the time of block zero.
const FIRST_HEAD: u64 = 100;
const GENESIS_TIME: u64 = 1_700_000_000;
const TTL: u64 = 72 * 3600;
/// How many reads of the record, at blocks past the one that showed it
/// set, answer empty, as from a fork that lost the record's transaction.
const EMPTY_READS: usize = 2;

#[derive(Default)]
struct Node {
    /// The newest block, moved on by one at each read of the head.
    head: u64,
    /// The signer the record names, and the block it was set at.
    set: Option<(Address, u64)>,
    /// Reads of the set record still to be answered empty.
    empty_left: usize,
    /// Reads of the set record answered empty so far.
    empty_served: usize,
    /// Reads of the set record answered truthfully after the empty ones.
    truthful_after: usize,
}

impl Node {
    fn time(block: u64) -> u64 {
        GENESIS_TIME + 2 * block
    }

    /// The record's signer as of `block`.
    fn signer_at(&self, block: u64) -> Option<(Address, u64)> {
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
        // The chain never logs a clear of any signer.
        "eth_getLogs" => ok(json!([])),
        "eth_blockNumber" => ok(json!(format!("0x{:x}", node.head))),
        "eth_getBlockByNumber" => match quantity(&params[0]) {
            // The block the chain treats as safe or final: no look begins.
            None if matches!(params[0].as_str(), Some("safe" | "finalized")) => {
                ok(block(node.head))
            }
            // A past block, by its number.
            Some(number) if number <= node.head => ok(block(number)),
            Some(_) => ok(Value::Null),
            // The head: a look begins.
            None => {
                node.head += 1;
                ok(block(node.head))
            }
        },
        "eth_call" => {
            // The block the read is pinned to, or the head.
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
            if params[0].get("to").is_none_or(Value::is_null)
                || params.get(2).is_some_and(|overrides| !overrides.is_null())
            {
                println!("the stand-in node refused an eth_call: {params}");
                return refused("not here");
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
                IAttestationRegistry::rootKeyCall::SELECTOR => match recorded {
                    None => ok(json!(record(Address::ZERO, 0))),
                    Some((signer, from)) => {
                        if node.empty_left > 0 {
                            // A fork that lost the record's transaction: the
                            // record is empty at this later block.
                            node.empty_left -= 1;
                            node.empty_served += 1;
                            return ok(json!(record(Address::ZERO, 0)));
                        }
                        if node.empty_served > 0 {
                            node.truthful_after += 1;
                        }
                        ok(json!(record(signer, Node::time(from))))
                    }
                },
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

/// An executor serving base alone, base being the canonical chain for the
/// root key, answered by the stand-in node.
async fn an_executor(node: SocketAddr) -> (status_port::Started, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "shonin-executor-root-reorg-{}-{}",
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

/// `GET /rootkey`, as the executor answers it.
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

/// Ask `/rootkey` until `done` holds of it, or `within` passes.
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

/// Wait until `done` holds of the node, or `within` passes.
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
    // The record is empty at two blocks in a row: the executor makes a root
    // and waits for the chain to record it.
    let made = status_until(port, Duration::from_secs(90), |status| status.holds_root)
        .await
        .filter(|status| status.holds_root)
        .ok_or("the executor never made a root")?;
    let first = made.signer.ok_or("a root held with no signer")?;

    // The chain records it, live, from the next block: the executor serves it.
    {
        let mut node = node.lock().unwrap();
        let from = node.head + 1;
        node.set = Some((first, from));
    }
    status_until(port, Duration::from_secs(60), |status| status.serving)
        .await
        .filter(|status| status.serving && status.signer == Some(first))
        .ok_or("the executor never served the recorded root")?;

    // A reorganisation a few blocks deep: the next reads of the record, at
    // blocks past the one that showed it set, find it empty, as a node on a
    // fork that lost the record's transaction answers. No clear is logged.
    node.lock().unwrap().empty_left = EMPTY_READS;
    if !node_until(node, Duration::from_secs(150), |node| {
        node.empty_served >= EMPTY_READS
    })
    .await
    {
        return Err("the executor never looked at the record again".into());
    }
    // The fork is gone: the record names the first root again, live.
    if !node_until(node, Duration::from_secs(120), |node| {
        node.truthful_after >= 1
    })
    .await
    {
        return Err("the executor never looked at the record after the empty reads".into());
    }
    // Give the executor a moment to act on what it read.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let now = root_status(port)
        .await
        .ok_or("the status port stopped answering")?;
    if !now.holds_root || now.signer != Some(first) || !now.serving {
        return Err(format!(
            "after empty reads at later blocks, with no clear logged, the executor no longer holds and serves the root the chain records ({first}): {now:?}"
        ));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_record_that_reads_empty_after_a_reorg_keeps_the_recorded_root_until_a_clear_is_logged() {
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
    assert!(
        !said.contains("the root key's record was cleared"),
        "empty reads at later blocks, with no clear in the chain's log, were taken for an admin's clear:\n{said}"
    );
}
