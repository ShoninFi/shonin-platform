//! A stand-in node: a chain whose blocks have hashes and parents, as the
//! chain follower checks them, with one vault on it, every read answered
//! with zeros unless the test's own answer says otherwise.
//!
//! The node refuses state overrides and has no Multicall3 (every address
//! has code, none Multicall3's), so the follower
//! reads each view on its own, pinned to the block's hash; it answers such a
//! read as of that block, and refuses a hash no block of its has. A test's
//! answer sees every call with the block it is pinned to written as that
//! block's number, as a test asks about blocks by number.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use alloy::primitives::{Address, B256, Log as Primitive, keccak256};
use alloy::sol_types::{SolCall, SolEvent};
use axum::Router;
use axum::routing::post;
use chain::{ChainConfig, ChainKey, Client, Deployments, Served, Signing};
use indexer::Indexer;
use serde_json::{Value, json};

pub const DEPLOYED: u64 = 1;
pub const VAULT: &str = "00000000000000000000000000000000000000be";

/// The head the stand-in is at unless a test moves it.
pub const HEAD: u64 = 0x10;

/// The time of every block unless a test dates them otherwise.
pub const TIME: u64 = 1_790_000_000;

pub fn vault() -> Address {
    format!("0x{VAULT}").parse().unwrap()
}

pub fn selector<C: SolCall>() -> String {
    format!("0x{}", alloy::hex::encode(C::SELECTOR))
}

/// Block `number`'s hash on the stand-in's chain: its last eight bytes are
/// the number, so a hash names its block at once.
pub fn block_hash(number: u64) -> B256 {
    hash_on(number, 0)
}

/// Block `number`'s hash on branch `branch` (0 is the first).
pub fn hash_on(number: u64, branch: u64) -> B256 {
    let mut hash = keccak256(
        [
            b"stand-in block ".as_slice(),
            &number.to_be_bytes(),
            &branch.to_be_bytes(),
        ]
        .concat(),
    );
    hash[24..].copy_from_slice(&number.to_be_bytes());
    hash
}

/// What a request asks for: the call's input, lowercased, for `eth_call`.
pub fn input(request: &Value) -> String {
    request["params"][0]["input"]
        .as_str()
        .or_else(|| request["params"][0]["data"].as_str())
        .unwrap_or("0x")
        .to_ascii_lowercase()
}

/// The block a call or a header read asks for, by number: `head` for the
/// head.
pub fn requested_block(request: &Value, head: u64) -> u64 {
    let at = match request["method"].as_str() {
        Some("eth_getBlockByNumber") => &request["params"][0],
        _ => &request["params"][1],
    };
    match at.as_str() {
        Some(tag) if tag.starts_with("0x") => quantity(tag),
        _ => head,
    }
}

pub fn quantity(text: &str) -> u64 {
    u64::from_str_radix(text.trim_start_matches("0x"), 16).unwrap_or(0)
}

/// Whether a log request is for the vault's own logs (among others).
pub fn asks_the_vault(request: &Value) -> bool {
    asks_for(request, vault())
}

/// Whether a log request names `address`, alone or among others.
pub fn asks_for(request: &Value, address: Address) -> bool {
    let wanted = format!("{address:#x}");
    match &request["params"][0]["address"] {
        Value::String(one) => one.eq_ignore_ascii_case(&wanted),
        Value::Array(many) => many.iter().any(|one| {
            one.as_str()
                .is_some_and(|one| one.eq_ignore_ascii_case(&wanted))
        }),
        _ => false,
    }
}

/// The first block a log request asks for, and the last.
pub fn log_range(request: &Value) -> (u64, u64) {
    let filter = &request["params"][0];
    let bound = |key: &str| quantity(filter[key].as_str().unwrap_or("0x0"));
    (bound("fromBlock"), bound("toBlock"))
}

/// The logs among `logs` a log request asks for: those of an address it
/// names, with a first topic it names, in its range.
pub fn matching(request: &Value, logs: &[Value]) -> Vec<Value> {
    let filter = &request["params"][0];
    let (from, to) = log_range(request);
    let same = |a: &Value, b: &Value| {
        a.as_str().map(str::to_lowercase) == b.as_str().map(str::to_lowercase)
    };
    let one_of = |wanted: &Value, value: &Value| match wanted {
        Value::Null => true,
        Value::Array(any) => any.iter().any(|w| same(w, value)),
        _ => same(wanted, value),
    };
    logs.iter()
        .filter(|log| {
            let block = quantity(log["blockNumber"].as_str().unwrap_or("0x0"));
            let topics = log["topics"].as_array().cloned().unwrap_or_default();
            from <= block
                && block <= to
                && one_of(&filter["address"], &log["address"])
                && filter["topics"]
                    .as_array()
                    .map(|wanted| {
                        wanted.iter().enumerate().all(|(i, wanted)| {
                            wanted.is_null()
                                || topics.get(i).is_some_and(|topic| one_of(wanted, topic))
                        })
                    })
                    .unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// A log of `address`'s, in `block` at `index`, in a transaction of its own
/// numbered by the block and index.
pub fn a_log<E: SolEvent>(address: Address, event: E, block: u64, index: u64) -> Value {
    let log = alloy::rpc::types::Log {
        inner: Primitive {
            address,
            data: event.encode_log_data(),
        },
        block_hash: Some(block_hash(block)),
        block_number: Some(block),
        block_timestamp: Some(TIME + block),
        transaction_hash: Some(keccak256(
            [&block.to_be_bytes()[..], &index.to_be_bytes()[..]].concat(),
        )),
        transaction_index: Some(0),
        log_index: Some(index),
        removed: false,
    };
    serde_json::to_value(log).unwrap()
}

/// A log of the vault's, in `block` at `index`.
pub fn logged<E: SolEvent>(event: E, block: u64, index: u64) -> Value {
    a_log(vault(), event, block, index)
}

/// What a test's answer starts with to have a call revert, rather than the
/// node fail to answer it.
pub const REVERTED: &str = "execution reverted";

/// A test's answer to a request: `Some(Ok(result))`, `Some(Err(message))`
/// for a JSON-RPC error (a revert when the message starts with
/// [`REVERTED`]), or `None` for the default.
pub type Answer = Arc<dyn Fn(&Value) -> Option<Result<Value, String>> + Send + Sync>;

/// How long a node takes over one request.
pub type Delay = Arc<dyn Fn(&Value) -> std::time::Duration + Send + Sync>;

/// The stand-in's chain: its id, its head, and each block's time.
pub struct Chain {
    pub id: u64,
    /// The branch it is on: every block above `fork_at` is the branch's
    /// own, those at or below it the first branch's.
    pub fork: std::sync::Mutex<(u64, u64)>,
    pub head: AtomicU64,
    /// How long it takes to answer anything but `eth_chainId`, in ms, on
    /// top of its node's own delay: a test slows it down this way.
    pub delay_ms: AtomicU64,
    /// How long it takes to answer one request, where a test says.
    pub delay_of: std::sync::Mutex<Option<Delay>>,
    pub time: Box<dyn Fn(u64) -> u64 + Send + Sync>,
}

impl Chain {
    pub fn at(head: u64) -> Arc<Self> {
        Self::dated(head, |_| TIME)
    }

    pub fn dated(head: u64, time: impl Fn(u64) -> u64 + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            id: 31_337,
            fork: std::sync::Mutex::new((u64::MAX, 0)),
            delay_ms: AtomicU64::new(0),
            delay_of: std::sync::Mutex::new(None),
            head: AtomicU64::new(head),
            time: Box::new(time),
        })
    }

    /// A chain with id `id` at `head`.
    pub fn numbered(id: u64, head: u64) -> Arc<Self> {
        Arc::new(Self {
            id,
            fork: std::sync::Mutex::new((u64::MAX, 0)),
            delay_ms: AtomicU64::new(0),
            delay_of: std::sync::Mutex::new(None),
            head: AtomicU64::new(head),
            time: Box::new(|_| TIME),
        })
    }

    pub fn head(&self) -> u64 {
        self.head.load(Ordering::SeqCst)
    }

    /// Block `number`'s hash on the branch the chain is on.
    pub fn hash(&self, number: u64) -> B256 {
        let (fork_at, branch) = *self.fork.lock().unwrap();
        hash_on(number, if number > fork_at { branch } else { 0 })
    }

    /// Replace every block above `fork_at` with branch `branch`'s, the head
    /// at `head`: a reorganisation.
    pub fn fork(&self, fork_at: u64, branch: u64, head: u64) {
        *self.fork.lock().unwrap() = (fork_at, branch);
        self.head.store(head, Ordering::SeqCst);
    }

    /// A log of `address`'s in `block` at `index`, on the branch the chain
    /// is on.
    pub fn log<E: SolEvent>(&self, address: Address, event: E, block: u64, index: u64) -> Value {
        let mut log = a_log(address, event, block, index);
        log["blockHash"] = json!(format!("{:#x}", self.hash(block)));
        log
    }

    /// Block `number`, as a header read gives it.
    pub fn block(&self, number: u64) -> Value {
        let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
        block.header.inner.number = number;
        block.header.inner.parent_hash = if number == 0 {
            B256::ZERO
        } else {
            self.hash(number - 1)
        };
        block.header.inner.timestamp = (self.time)(number);
        block.header.hash = self.hash(number);
        serde_json::to_value(block).unwrap()
    }

    /// The number of the block whose hash is `hash`, if it is one of the
    /// chain's.
    fn number_of(&self, hash: &str) -> Option<u64> {
        let hash: B256 = hash.parse().ok()?;
        let number = u64::from_be_bytes(hash[24..].try_into().ok()?);
        (number <= self.head() && self.hash(number) == hash).then_some(number)
    }
}

pub async fn a_node(answer: Answer) -> String {
    a_node_on(Chain::at(HEAD), answer).await
}

/// A stand-in node on `chain`.
pub async fn a_node_on(chain: Arc<Chain>, answer: Answer) -> String {
    a_slow_node_on(chain, std::time::Duration::ZERO, answer).await
}

/// A stand-in node on `chain` that answers every request but `eth_chainId`
/// after `delay`.
pub async fn a_slow_node_on(
    chain: Arc<Chain>,
    delay: std::time::Duration,
    answer: Answer,
) -> String {
    let app = Router::new().route(
        "/",
        post(move |axum::Json(request): axum::Json<Value>| {
            let answer = answer.clone();
            let chain = chain.clone();
            async move {
                let delay_of = chain.delay_of.lock().unwrap().clone();
                let delay = delay
                    .max(std::time::Duration::from_millis(
                        chain.delay_ms.load(Ordering::SeqCst),
                    ))
                    .max(delay_of.map_or(std::time::Duration::ZERO, |of| of(&request)));
                if request["method"] != "eth_chainId" && !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                axum::Json(reply(&chain, &answer, request))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}")
}

fn reply(chain: &Chain, answer: &Answer, mut request: Value) -> Value {
    let id = request["id"].clone();
    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
    let error = |code: i64, message: &str| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
    let method = request["method"].as_str().unwrap_or_default().to_string();
    let head = chain.head();
    match method.as_str() {
        "eth_chainId" => return ok(json!(format!("{:#x}", chain.id))),
        // Every contract has code (none Multicall3's).
        "eth_getCode" => return ok(json!("0x01")),
        "eth_getBlockByHash" => {
            let found = request["params"][0]
                .as_str()
                .and_then(|hash| chain.number_of(hash));
            return ok(found.map_or(Value::Null, |number| chain.block(number)));
        }
        "eth_call" => {
            if !request["params"][2].is_null() {
                return error(-32602, "state overrides are not supported");
            }
            // The block it is pinned to, as a number.
            let pinned = &request["params"][1];
            let number = match pinned {
                Value::Object(pin) => match pin.get("blockHash").and_then(Value::as_str) {
                    Some(hash) => match chain.number_of(hash) {
                        Some(number) => number,
                        None => return error(-32001, "header not found"),
                    },
                    None => pin
                        .get("blockNumber")
                        .and_then(Value::as_str)
                        .map_or(head, quantity),
                },
                Value::String(tag) if tag.starts_with("0x") => quantity(tag),
                _ => head,
            };
            if number > head {
                return error(-32000, "header not found");
            }
            request["params"][1] = json!(format!("{number:#x}"));
            let to = &request["params"][0]["to"];
            if to.is_null() {
                // Code run as a creation: the block's number, or the gas.
                let code = input(&request);
                return if code.starts_with("0x43") {
                    ok(json!(format!("0x{number:064x}")))
                } else {
                    ok(json!(format!("0x{:064x}", 30_000_000u64)))
                };
            }
            if to.as_str().is_some_and(|to| {
                to.eq_ignore_ascii_case(&format!("{:#x}", Address::with_last_byte(0x64)))
            }) {
                return error(-32000, "execution reverted");
            }
        }
        _ => {}
    }
    match answer(&request) {
        Some(Ok(result)) => ok(result),
        // A revert, as a node gives one: the call's own answer.
        Some(Err(message)) if message.starts_with(REVERTED) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": 3, "message": message, "data": "0x"}
        }),
        Some(Err(message)) => error(-32000, &message),
        None => match method.as_str() {
            "eth_blockNumber" => ok(json!(format!("{head:#x}"))),
            "eth_getBlockByNumber" => {
                let number = requested_block(&request, head);
                ok(if number > head {
                    Value::Null
                } else {
                    chain.block(number)
                })
            }
            "eth_getBalance" => ok(json!("0x0")),
            "eth_call" => {
                let data = input(&request);
                if data == selector::<chain::bindings::factory::IVaultFactory::vaultsCall>() {
                    ok(json!(format!(
                        "0x{}{}{}",
                        "0".repeat(62) + "20",
                        "0".repeat(63) + "1",
                        "0".repeat(24) + VAULT
                    )))
                } else {
                    ok(json!(format!("0x{}", "0".repeat(64 * 128))))
                }
            }
            _ => ok(json!([])),
        },
    }
}

pub fn deployments() -> Deployments {
    deployments_from(DEPLOYED)
}

/// The platform's deployment, made at `deployed`: every contract at one
/// address.
pub fn deployments_from(deployed: u64) -> Deployments {
    deployments_on(31_337, deployed)
}

/// The platform's deployment on chain `chain_id`, made at `deployed`.
pub fn deployments_on(chain_id: u64, deployed: u64) -> Deployments {
    let one = "0x0000000000000000000000000000000000000001";
    let mut fields = vec![
        format!(r#""chain_key":"evm:{chain_id}""#),
        format!(r#""chain_id":{chain_id}"#),
        r#""rpc_url":"""#.to_string(),
        format!(r#""deployed_block":{deployed}"#),
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
    Deployments::from_json(&format!("{{{}}}", fields.join(","))).unwrap()
}

/// A client of the node at `node`.
pub async fn client(node: &str) -> Client {
    let config = ChainConfig {
        label: "stand-in".to_string(),
        rpc: node.to_string(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        public_rpc: node.to_string(),
        logs_rpc: None,
        log_range: None,
        confirmations: None,
    };
    Client::connect(&config, Signing::ReadOnly).await.unwrap()
}

/// The checked-in settings, with a read budget roomy enough for a busy
/// test machine.
pub fn policy() -> chain::rpc_policy::RpcPolicy {
    chain::rpc_policy::RpcPolicy {
        read_budget_ms: 10_000,
        ..chain::rpc_policy::RpcPolicies::established()
            .for_chain(&"evm:31337".parse::<ChainKey>().unwrap())
    }
}

pub async fn an_indexer(answer: Answer, name: &str) -> Arc<Indexer> {
    an_indexer_on(Chain::at(HEAD), deployments(), answer, name).await
}

/// An indexer reading Base from a stand-in node on `chain`, the platform
/// deployed as `deployments` says.
pub async fn an_indexer_on(
    chain: Arc<Chain>,
    deployments: Deployments,
    answer: Answer,
    name: &str,
) -> Arc<Indexer> {
    let node = a_node_on(chain, answer).await;
    let client = client(&node).await;
    let store = std::env::temp_dir().join(format!(
        "shonin-indexer-stand-in-{name}-{}",
        std::process::id()
    ));
    let indexer =
        Arc::new(Indexer::new(vec![shared::Chain::Base], store, None).with_rpc_policy(policy()));
    indexer
        .add(Served {
            vault_chain: shared::Chain::Base,
            key: "evm:31337".parse::<ChainKey>().unwrap(),
            client,
            deployments,
            native_usd: None,
            public_rpc: String::new(),
        })
        .await;
    indexer
}

/// Base, as a stand-in node on `chain` serves it.
pub async fn served_on(chain: Arc<Chain>, deployments: Deployments, answer: Answer) -> Served {
    served_as(
        shared::Chain::Base,
        chain,
        std::time::Duration::ZERO,
        deployments,
        answer,
    )
    .await
}

/// `vault_chain`, as a stand-in node on `chain` serves it, answering every
/// request but `eth_chainId` after `delay`.
pub async fn served_as(
    vault_chain: shared::Chain,
    chain: Arc<Chain>,
    delay: std::time::Duration,
    deployments: Deployments,
    answer: Answer,
) -> Served {
    let key = format!("evm:{}", chain.id).parse::<ChainKey>().unwrap();
    let node = a_slow_node_on(chain, delay, answer).await;
    Served {
        vault_chain,
        key,
        client: client(&node).await,
        deployments,
        native_usd: None,
        public_rpc: String::new(),
    }
}

/// What `vaults()` answers when the factory has made none.
pub fn no_vaults() -> Value {
    json!(format!("0x{}{}", "0".repeat(62) + "20", "0".repeat(64)))
}

/// An answer that has the factory list no vault.
pub fn answering_no_vaults() -> Answer {
    Arc::new(|request: &Value| {
        (request["method"] == "eth_call"
            && input(request) == selector::<chain::bindings::factory::IVaultFactory::vaultsCall>())
        .then(|| Ok(no_vaults()))
    })
}
