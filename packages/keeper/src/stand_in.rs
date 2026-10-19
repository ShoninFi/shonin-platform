//! A stand-in node for the keeper's tests, whose answers are scripted and
//! which records every transaction the keeper sends.
//!
//! It answers JSON-RPC over HTTP with axum, which the keeper already depends
//! on, so nothing is added to the crate. `eth_call` is answered by (contract,
//! selector) from a script; a call the script doesn't cover answers as a
//! revert, so a path a test didn't foresee shows up as one. The keeper's
//! simulation of a stopped station's unwind and its value after (creation
//! code run by `eth_call`, or the same code given to an address by a state
//! override) is answered from the station's scripted unwind, and the mark
//! [`leaves_after_unwind`] scripts, or else its scripted mark less what the
//! unwind brings home; [`refuses_creation_calls`] has it refuse the former,
//! and [`refuses_simulations`] both.
//! A call run at a set block time (a send simulated at the moment it would
//! land) goes through unless the send is refused, or it falls outside a
//! window [`keeps_the_window`] scripts; Multicall3's read of the block's
//! time answers the time set. [`ignores_block_time`] has the node run such a
//! call at the head's time instead, and [`refuses_block_time`] refuse it.
//! `eth_sendTransaction` is recorded and mined at once with a successful
//! receipt: whether a send would revert on a real chain is not what these
//! tests judge; what the keeper chose to send is. [`connect_with_blocks_of`]
//! puts a clock in front of it, for tests where when a send lands matters.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, B256, FixedBytes, keccak256};
use alloy::sol_types::SolCall;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use chain::bindings::vault::Timers;
use chain::{ChainConfig, Client, Deployments, Roles, Signing};
use serde_json::{Value, json};

use crate::DAY;

type Selector = [u8; 4];

/// What the stand-in node answers and what it has been sent.
#[derive(Default)]
pub struct Node {
    /// The chain's id: each stand-in's own (see [`node`]).
    pub chain_id: u64,
    /// The head block's timestamp.
    pub timestamp: u64,
    /// The chain's head block.
    pub head: u64,
    /// How many seconds apart its blocks are: a block before the head is
    /// that much older for each block back.
    pub block_time: u64,
    /// The widest range of blocks it reads logs for; a wider one is refused.
    pub most_log_blocks: Option<u64>,
    /// The widest range of blocks it reads logs for before it gives up and
    /// answers with an HTTP error instead, as a node behind a gateway does
    /// when a read takes too long.
    pub most_log_blocks_answered: Option<u64>,
    /// The ranges it read logs for, in order.
    pub log_reads: Vec<(u64, u64)>,
    answers: HashMap<(Address, Selector), Result<Vec<u8>, String>>,
    /// Calls it refuses to take, as a node does when their gas is estimated,
    /// with the reason it gives, if any.
    refused: HashMap<(Address, Selector), String>,
    /// Each transaction taken, with the head's timestamp when it was sent.
    sent: Vec<(Address, Vec<u8>, u64)>,
    /// What each station's mark would be after its full unwind.
    after_unwind: HashMap<Address, alloy::primitives::U256>,
    /// Whether it refuses a call with no contract to call, and one with a
    /// state override.
    no_creation_calls: bool,
    no_override_calls: bool,
    /// The gas limit each transaction taken was sent with, if any.
    gas_limits: Vec<(Address, Selector, Option<u64>)>,
    /// Each send run at a set moment, and the moment.
    simulated_at: Vec<(Address, Selector, u64)>,
    /// The slot interval and window a send run at a set moment must land
    /// inside, if any.
    window: Option<(u64, u64)>,
    /// Whether it runs a call given a block time at the head's time instead,
    /// and whether it refuses one.
    ignores_block_time: bool,
    refuses_block_time: bool,
    /// How many times it was asked the block's time with a time set.
    pub block_time_probes: usize,
    /// How many blocks behind the node answering reads by number is, while
    /// `latest` is answered at the head: a load-balanced endpoint whose next
    /// request reaches a node behind, which has no such block.
    pub numbers_behind: u64,
    /// Every request it was asked, by method, with the contract and
    /// selector of each plain `eth_call`.
    pub asked: Vec<(String, Option<(Address, Selector)>)>,
}

pub type Shared = Arc<Mutex<Node>>;

/// A stand-in node, on a chain of its own: what the keeper keeps across
/// rounds by chain id and vault (in its statics, for its health) is then
/// each test's own, however many run at once.
pub fn node(timestamp: u64) -> Shared {
    static CHAIN_IDS: AtomicU64 = AtomicU64::new(1_000_000);
    Arc::new(Mutex::new(Node {
        chain_id: CHAIN_IDS.fetch_add(1, Ordering::Relaxed),
        timestamp,
        head: 16,
        ..Node::default()
    }))
}

fn selector_of(data: &[u8]) -> Selector {
    data[..4].try_into().expect("calldata carries a selector")
}

fn tx_fields(params: &Value) -> (Address, Vec<u8>) {
    let tx = &params[0];
    let to: Address = tx["to"]
        .as_str()
        .expect("a call names its contract")
        .parse()
        .expect("an address");
    let data = tx
        .get("input")
        .or_else(|| tx.get("data"))
        .and_then(Value::as_str)
        .unwrap_or("0x");
    (to, alloy::hex::decode(data).expect("hex calldata"))
}

fn answer(node: &Shared, request: &Value) -> Value {
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or_default();
    let params = &request["params"];
    let mut node = node.lock().unwrap();
    let plain_call = (method == "eth_call"
        && params.get(2).is_none_or(Value::is_null)
        && params.get(3).is_none_or(Value::is_null)
        && params[0].get("to").is_some_and(|to| !to.is_null()))
    .then(|| {
        let (to, data) = tx_fields(params);
        (
            to,
            data.get(..4)
                .and_then(|four| four.try_into().ok())
                .unwrap_or_default(),
        )
    });
    node.asked.push((method.to_string(), plain_call));
    let result: Result<Value, String> = match method {
        "eth_chainId" => Ok(json!(format!("0x{:x}", node.chain_id))),
        "eth_accounts" => Ok(json!([])),
        "eth_blockNumber" => Ok(json!(format!("0x{:x}", node.head))),
        "eth_getBlockByNumber"
            if params[0]
                .as_str()
                .and_then(|asked| asked.strip_prefix("0x"))
                .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                .is_some_and(|number| number + node.numbers_behind > node.head) =>
        {
            Ok(Value::Null)
        }
        "eth_getBlockByNumber" => {
            let number = params[0]
                .as_str()
                .and_then(|asked| asked.strip_prefix("0x"))
                .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                .unwrap_or(node.head)
                .min(node.head);
            let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
            block.header.inner.number = number;
            block.header.inner.timestamp = node
                .timestamp
                .saturating_sub((node.head - number) * node.block_time);
            block.header.inner.base_fee_per_gas = Some(1_000_000_000);
            Ok(serde_json::to_value(block).unwrap())
        }
        "eth_gasPrice" | "eth_maxPriorityFeePerGas" => Ok(json!("0x3b9aca00")),
        "eth_estimateGas" => Ok(json!("0x5208")),
        "eth_getLogs" => {
            let block = |name: &str| {
                u64::from_str_radix(
                    params[0][name]
                        .as_str()
                        .expect("a log read names its blocks")
                        .trim_start_matches("0x"),
                    16,
                )
                .expect("a block number")
            };
            let (from, to) = (block("fromBlock"), block("toBlock"));
            if node
                .most_log_blocks
                .is_some_and(|most| to - from + 1 > most)
            {
                Err("block range is too wide".to_string())
            } else {
                node.log_reads.push((from, to));
                Ok(json!([]))
            }
        }
        // A send run at the moment it would land, the block's time set: it
        // goes through unless the node refuses the send, or it would land
        // outside the window [`keeps_the_window`] scripts.
        "eth_call" if params.get(3).is_some_and(Value::is_object) => {
            if node.refuses_block_time {
                return json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "block overrides are not supported"}});
            }
            let (to, data) = tx_fields(params);
            let key = (to, selector_of(&data));
            let given = params[3]["time"]
                .as_str()
                .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
                .unwrap_or(node.timestamp);
            let time = if node.ignores_block_time {
                node.timestamp
            } else {
                given
            };
            if to == crate::MULTICALL3 {
                node.block_time_probes += 1;
                let time = alloy::primitives::U256::from(time);
                return json!({"jsonrpc": "2.0", "id": id, "result": alloy::hex::encode_prefixed(time.to_be_bytes::<32>())});
            }
            node.simulated_at.push((to, key.1, time));
            if let Some(reason) = node.refused.get(&key) {
                Err(format!("execution reverted: {reason}"))
            } else if node
                .window
                .is_some_and(|(interval, window)| time % interval.max(1) > window)
            {
                Err("execution reverted: OutsideWindow()".to_string())
            } else {
                Ok(json!("0x"))
            }
        }
        "eth_call" if params[0].get("to").is_none_or(Value::is_null) => {
            if node.no_creation_calls {
                Err("a call needs a contract to call".to_string())
            } else {
                simulate_unwind(&node, &call_data(&params[0]))
            }
        }
        // The probe as an overridden contract's code.
        "eth_call" if params.get(2).is_some_and(Value::is_object) => {
            if node.no_override_calls {
                return json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "state overrides are not supported"}});
            }
            let code = params[2]
                .as_object()
                .and_then(|overrides| overrides.values().next())
                .and_then(|account| account["code"].as_str())
                .and_then(|hex| alloy::hex::decode(hex).ok())
                .unwrap_or_default();
            simulate_unwind(&node, &code)
        }
        "eth_call" => {
            let (to, data) = tx_fields(params);
            let key = (to, selector_of(&data));
            match node.answers.get(&key) {
                Some(Ok(bytes)) => Ok(json!(alloy::hex::encode_prefixed(bytes))),
                Some(Err(reason)) => Err(format!("execution reverted: {reason}")),
                None => Err(format!(
                    "the stand-in was not told how to answer {} at {to}",
                    alloy::hex::encode_prefixed(key.1)
                )),
            }
        }
        "eth_sendTransaction"
            if node.refused.contains_key(&{
                let (to, data) = tx_fields(params);
                (to, selector_of(&data))
            }) =>
        {
            let (to, data) = tx_fields(params);
            match node.refused[&(to, selector_of(&data))].as_str() {
                "" => Err("execution reverted".to_string()),
                reason => Err(format!("execution reverted: {reason}")),
            }
        }
        "eth_sendTransaction" => {
            let (to, data) = tx_fields(params);
            let gas = params[0]["gas"]
                .as_str()
                .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok());
            node.gas_limits.push((to, selector_of(&data), gas));
            let hash = keccak256([&(node.sent.len() as u64).to_be_bytes()[..], &data].concat());
            let at = node.timestamp;
            node.sent.push((to, data, at));
            Ok(json!(hash))
        }
        "eth_getTransactionReceipt" => Ok(json!({
            "transactionHash": params[0],
            "transactionIndex": "0x0",
            "blockHash": B256::repeat_byte(0xbb),
            "blockNumber": "0x11",
            "from": Address::ZERO,
            "to": Address::repeat_byte(0x22),
            "cumulativeGasUsed": "0x5208",
            "gasUsed": "0x5208",
            "effectiveGasPrice": "0x3b9aca00",
            "contractAddress": null,
            "logs": [],
            "logsBloom": format!("0x{}", "00".repeat(256)),
            "status": "0x1",
            "type": "0x2",
        })),
        other => Err(format!("the stand-in node was asked {other}")),
    };
    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(message) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": 3, "message": message}})
        }
    }
}

/// Answer the keeper's simulation of a station's full unwind and its value
/// after, for whichever scripted station its creation code names.
fn simulate_unwind(node: &Node, code: &[u8]) -> Result<Value, String> {
    use alloy::primitives::U256;
    use chain::bindings::station::IStation;
    // Nothing lives where the keeper checks that the node simulates: the
    // calls go through and return nothing.
    if code == crate::unwind_probe(crate::NOTHING_AT) {
        return Ok(json!(alloy::hex::encode_prefixed([0u8; 96])));
    }
    let station = node
        .answers
        .keys()
        .map(|(at, _)| *at)
        .find(|at| crate::unwind_probe(*at) == code)
        .ok_or("the stand-in was not told of the station this simulates")?;
    let unwound = match node.answers.get(&(station, IStation::unwindCall::SELECTOR)) {
        Some(Ok(bytes)) => {
            IStation::unwindCall::abi_decode_returns(bytes).map_err(|e| e.to_string())?
        }
        Some(Err(reason)) => return Err(format!("execution reverted: {reason}")),
        None => return Err("the stand-in was not told how the station unwinds".to_string()),
    };
    let mark = match node.after_unwind.get(&station) {
        Some(mark) => *mark,
        None => match node.answers.get(&(station, IStation::valueCall::SELECTOR)) {
            Some(Ok(bytes)) => {
                let value =
                    IStation::valueCall::abi_decode_returns(bytes).map_err(|e| e.to_string())?;
                if unwound.blocked {
                    value.mark
                } else {
                    value.mark.saturating_sub(unwound.sentHome)
                }
            }
            Some(Err(reason)) => return Err(format!("execution reverted: {reason}")),
            None => return Err("the stand-in was not told the station's value".to_string()),
        },
    };
    let mut returned = Vec::new();
    returned.extend_from_slice(&unwound.sentHome.to_be_bytes::<32>());
    returned.extend_from_slice(&U256::from(u8::from(unwound.blocked)).to_be_bytes::<32>());
    returned.extend_from_slice(&mark.to_be_bytes::<32>());
    Ok(json!(alloy::hex::encode_prefixed(returned)))
}

/// A call's data.
fn call_data(tx: &Value) -> Vec<u8> {
    tx.get("input")
        .or_else(|| tx.get("data"))
        .and_then(Value::as_str)
        .and_then(|hex| alloy::hex::decode(hex).ok())
        .unwrap_or_default()
}

/// Have the node refuse a call with no contract to call, as a gateway that
/// won't run creation code by `eth_call` does.
pub fn refuses_creation_calls(node: &Shared) {
    node.lock().unwrap().no_creation_calls = true;
}

/// Have the node refuse the simulation in both its forms: a call with no
/// contract to call, and one with a state override.
pub fn refuses_simulations(node: &Shared) {
    let mut node = node.lock().unwrap();
    node.no_creation_calls = true;
    node.no_override_calls = true;
}

/// Have the node run both again.
pub fn takes_simulations(node: &Shared) {
    let mut node = node.lock().unwrap();
    node.no_creation_calls = false;
    node.no_override_calls = false;
}

/// The gas limit each call `C` taken was sent with, if one was given.
pub fn gas_limits_of<C: SolCall>(node: &Shared, to: Address) -> Vec<Option<u64>> {
    node.lock()
        .unwrap()
        .gas_limits
        .iter()
        .filter(|(at, selector, _)| *at == to && *selector == C::SELECTOR)
        .map(|(.., gas)| *gas)
        .collect()
}

/// Have a send run at a set moment refused when that moment falls outside
/// the window of a slot `interval` seconds long, as the vault refuses a
/// valuation that lands late.
pub fn keeps_the_window(node: &Shared, interval: u64, window: u64) {
    node.lock().unwrap().window = Some((interval, window));
}

/// Have the node run a call given a block time at the head's time instead,
/// without an error, as some nodes do.
pub fn ignores_block_time(node: &Shared) {
    node.lock().unwrap().ignores_block_time = true;
}

/// Have the node refuse a call given a block time.
pub fn refuses_block_time(node: &Shared) {
    node.lock().unwrap().refuses_block_time = true;
}

/// The moments each call `C` was run at before it was sent.
pub fn simulated_at<C: SolCall>(node: &Shared, to: Address) -> Vec<u64> {
    node.lock()
        .unwrap()
        .simulated_at
        .iter()
        .filter(|(at, selector, _)| *at == to && *selector == C::SELECTOR)
        .map(|(.., time)| *time)
        .collect()
}

/// Script what a station's mark would be after its full unwind.
pub fn leaves_after_unwind(node: &Shared, station: Address, mark: u64) {
    node.lock()
        .unwrap()
        .after_unwind
        .insert(station, alloy::primitives::U256::from(mark));
}

/// Whether a request is a log read wider than the node answers at all.
fn unanswerable(node: &Shared, request: &Value) -> bool {
    let Some(most) = node.lock().unwrap().most_log_blocks_answered else {
        return false;
    };
    if request["method"] != "eth_getLogs" {
        return false;
    }
    let block = |name: &str| {
        request["params"][0][name]
            .as_str()
            .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
            .unwrap_or_default()
    };
    block("toBlock") - block("fromBlock") + 1 > most
}

async fn handle(State(node): State<Shared>, Json(body): Json<Value>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let asked: Vec<&Value> = match &body {
        Value::Array(batch) => batch.iter().collect(),
        one => vec![one],
    };
    if asked.iter().any(|one| unanswerable(&node, one)) {
        return axum::http::StatusCode::BAD_GATEWAY.into_response();
    }
    Json(match &body {
        Value::Array(batch) => Value::Array(batch.iter().map(|one| answer(&node, one)).collect()),
        one => answer(&node, one),
    })
    .into_response()
}

pub async fn connect(node: &Shared) -> crate::Observed {
    let app = Router::new()
        .route("/", post(handle))
        .with_state(node.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = ChainConfig {
        label: "stand-in".to_string(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        confirmations: None,
        public_rpc: url,
        logs_rpc: None,
        log_range: None,
    };
    Client::connect(&config, Signing::ReadOnly)
        .await
        .unwrap()
        .into()
}

/// A client whose every mined transaction moves the stand-in's head on by
/// one block, and its clock by a block's time, as a real chain's next block
/// does: the keeper's next read of
/// the head sees the block its own transaction landed in. A proxy in front of
/// the stand-in moves the clock before it answers the send, and the blocks
/// before the head are as far apart.
pub async fn connect_with_blocks_of(node: &Shared, seconds: u64) -> crate::Observed {
    node.lock().unwrap().block_time = seconds;
    let behind = Behind::new(connect(node).await.rpc_url.clone());
    let state = (node.clone(), behind, seconds);
    let app = Router::new()
        .route(
            "/",
            post(
                |State((node, behind, seconds)): State<(Shared, Behind, u64)>,
                 Json(body): Json<Value>| async move {
                    let answer: Value = behind.forward(&body).await;
                    // Only a send the node took is mined; one it refused
                    // (its estimate reverted) takes no block.
                    let mined = |asked: &Value, answered: &Value| {
                        asked["method"] == "eth_sendTransaction" && answered.get("error").is_none()
                    };
                    let blocks = match (&body, &answer) {
                        (Value::Array(asked), Value::Array(answered)) => asked
                            .iter()
                            .zip(answered)
                            .filter(|(asked, answered)| mined(asked, answered))
                            .count(),
                        (asked, answered) => usize::from(mined(asked, answered)),
                    } as u64;
                    {
                        let mut node = node.lock().unwrap();
                        node.timestamp += seconds * blocks;
                        node.head += blocks;
                    }
                    Json(answer)
                },
            ),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = ChainConfig {
        label: "stand-in behind a clock".to_string(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        confirmations: None,
        public_rpc: url,
        logs_rpc: None,
        log_range: None,
    };
    Client::connect(&config, Signing::ReadOnly)
        .await
        .unwrap()
        .into()
}

/// A client whose sends are mined a block at a time, as a real chain mines
/// them: every `every` of the wall clock the stand-in's head moves on a
/// block and its clock by `seconds`, and every send taken since lands in
/// that block. A send's receipt is there only once its block is. Sends made
/// while the same head stands land in the same block, so the head's time a
/// send was made at (see [`everything_sent_when`]) names its block.
pub async fn connect_with_a_block_clock(
    node: &Shared,
    seconds: u64,
    every: std::time::Duration,
) -> crate::Observed {
    node.lock().unwrap().block_time = seconds;
    let behind = Behind::new(connect(node).await.rpc_url.clone());
    let pending: Arc<Mutex<Vec<Value>>> = Arc::default();
    {
        let (node, pending) = (node.clone(), pending.clone());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let mut node = node.lock().unwrap();
                node.timestamp += seconds;
                node.head += 1;
                pending.lock().unwrap().clear();
            }
        });
    }
    let state = (behind, pending);
    let app = Router::new()
        .route(
            "/",
            post(
                |State((behind, pending)): State<(Behind, Arc<Mutex<Vec<Value>>>)>,
                 Json(body): Json<Value>| async move {
                    if body["method"] == "eth_getTransactionReceipt"
                        && pending.lock().unwrap().contains(&body["params"][0])
                    {
                        return Json(json!({"jsonrpc": "2.0", "id": body["id"], "result": null}));
                    }
                    let answer: Value = behind.forward(&body).await;
                    if body["method"] == "eth_sendTransaction" && answer.get("error").is_none() {
                        pending.lock().unwrap().push(answer["result"].clone());
                    }
                    Json(answer)
                },
            ),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = ChainConfig {
        label: "stand-in mining blocks".to_string(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        confirmations: None,
        public_rpc: url,
        logs_rpc: None,
        log_range: None,
    };
    let client = Client::connect(&config, Signing::ReadOnly).await.unwrap();
    crate::receipts_polled_every(&client, std::time::Duration::from_millis(50));
    client.into()
}

/// The stand-in behind a proxy, asked over one pool of connections: a new
/// connection for every request would run the machine out of local ports
/// when many tests run at once.
#[derive(Clone)]
pub struct Behind {
    url: String,
    http: reqwest::Client,
}

impl Behind {
    fn new(url: String) -> Self {
        Self {
            url,
            http: reqwest::Client::new(),
        }
    }

    async fn forward(&self, asked: &Value) -> Value {
        self.http
            .post(&self.url)
            .json(asked)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

/// Script one call's answer.
pub fn answers<C: SolCall>(node: &Shared, to: Address, _call: C, ret: C::Return) {
    node.lock()
        .unwrap()
        .answers
        .insert((to, C::SELECTOR), Ok(C::abi_encode_returns(&ret)));
}

/// Script one call to revert.
pub fn reverts<C: SolCall>(node: &Shared, to: Address, _call: C, reason: &str) {
    node.lock()
        .unwrap()
        .answers
        .insert((to, C::SELECTOR), Err(reason.to_string()));
}

/// Have the node refuse to take this call, as it does one whose gas
/// estimate reverts.
pub fn refuses<C: SolCall>(node: &Shared, to: Address, call: C) {
    refuses_as(node, to, call, "");
}

/// Have the node refuse to take this call with the vault's reason, as the
/// estimate of one the vault reverts with that error says.
pub fn refuses_as<C: SolCall>(node: &Shared, to: Address, _call: C, reason: &str) {
    node.lock()
        .unwrap()
        .refused
        .insert((to, C::SELECTOR), reason.to_string());
}

/// Have the node take a call it was refusing.
pub fn takes<C: SolCall>(node: &Shared, to: Address, _call: C) {
    node.lock().unwrap().refused.remove(&(to, C::SELECTOR));
}

/// An executor that serves one signed total for every vault asked about,
/// signed at whatever `as_of` holds when it is asked.
pub async fn executor(total: u64, as_of: Arc<AtomicU64>) -> String {
    moving_executor(Arc::new(AtomicU64::new(total)), as_of).await
}

/// The same, its total whatever `total` holds when it is asked.
pub async fn moving_executor(total: Arc<AtomicU64>, as_of: Arc<AtomicU64>) -> String {
    let app = Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || {
            let total = total.load(Ordering::SeqCst);
            let as_of = as_of.load(Ordering::SeqCst);
            async move {
                Json(json!({
                    "total": total.to_string(),
                    "as_of": as_of,
                    "signature": format!("0x{}", "11".repeat(65)),
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    url
}

/// An executor whose first `failures` answers are a 503 (a restart behind a
/// proxy, a request that timed out), and which then serves `total` signed
/// at `as_of`.
pub async fn failing_executor(failures: usize, total: u64, as_of: u64) -> String {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || {
            let asked = asked.clone();
            async move {
                use axum::response::IntoResponse;
                if asked.fetch_add(1, Ordering::SeqCst) < failures {
                    return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                Json(json!({
                    "total": total.to_string(),
                    "as_of": as_of,
                    "signature": format!("0x{}", "11".repeat(65)),
                }))
                .into_response()
            }
        }),
    );
    serve(app).await
}

/// An executor that serves no total for any vault: every ask is a 404, as
/// one started again after it let a closed vault go answers.
pub async fn executor_serving_nothing() -> String {
    serve(Router::new()).await
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    url
}

/// The transactions sent to `to` with this call's selector.
pub fn sent<C: SolCall>(node: &Shared, to: Address) -> usize {
    node.lock()
        .unwrap()
        .sent
        .iter()
        .filter(|(at, data, _)| *at == to && selector_of(data) == C::SELECTOR)
        .count()
}

/// The head's timestamp as each transaction sent to `to` with this call's
/// selector was sent.
pub fn sent_at<C: SolCall>(node: &Shared, to: Address) -> Vec<u64> {
    node.lock()
        .unwrap()
        .sent
        .iter()
        .filter(|(at, data, _)| *at == to && selector_of(data) == C::SELECTOR)
        .map(|(_, _, when)| *when)
        .collect()
}

/// Every transaction taken, as [`everything_sent`] says it, with the
/// head's time it was sent at and its whole calldata.
pub fn everything_sent_when(node: &Shared) -> Vec<(String, u64)> {
    node.lock()
        .unwrap()
        .sent
        .iter()
        .map(|(to, data, at)| (format!("{to} {}", alloy::hex::encode_prefixed(data)), *at))
        .collect()
}

pub fn everything_sent(node: &Shared) -> Vec<String> {
    node.lock()
        .unwrap()
        .sent
        .iter()
        .map(|(to, data, _)| format!("{to} {}", alloy::hex::encode_prefixed(selector_of(data))))
        .collect()
}

pub fn timers() -> Timers {
    Timers {
        slotInterval: 10,
        slotLead: 5,
        slotWindow: 3,
        postDeadline: 60,
        transitTimeout: 3600,
        fallbackAfter: DAY,
        usageFreshness: 600,
        tankEscapeDelay: DAY,
        floorDelay: DAY,
        haltGrace: 600,
        freezeAfter: 7 * DAY,
        windDown: 7 * DAY,
        activationWindow: 3 * DAY,
    }
}

pub fn name(text: &str) -> FixedBytes<32> {
    FixedBytes::<32>::right_padding_from(text.as_bytes())
}

/// A moment outside any slot's valuation window, so neither a valuation nor
/// the one-transaction close is attempted and the step-by-step path is what
/// runs.
pub const BETWEEN_SLOTS: u64 = 1_700_000_005;

/// A moment at a slot's start: a valuation, or the one-transaction close,
/// sent then lands inside the window.
pub const SLOT_START: u64 = 1_700_000_000;

pub const VAULT: Address = Address::repeat_byte(0x11);
pub const STATION: Address = Address::repeat_byte(0x22);
pub const KEEPER: Address = Address::repeat_byte(0x33);

/// A deployment with nothing at any address but the keeper's role.
pub fn deployments() -> Deployments {
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
        factory: nobody,
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
            keeper: KEEPER,
            operator: nobody,
            author: nobody,
            viewer: nobody,
        },
    }
}

/// The calls sent to `to` with this call's selector, decoded.
pub fn sent_calls<C: SolCall>(node: &Shared, to: Address) -> Vec<C> {
    node.lock()
        .unwrap()
        .sent
        .iter()
        .filter(|(at, data, _)| *at == to && selector_of(data) == C::SELECTOR)
        .filter_map(|(_, data, _)| C::abi_decode(data).ok())
        .collect()
}

/// A client whose log reads naming `address` answer with `logs`, whatever
/// range they ask for; everything else goes to the stand-in.
pub async fn connect_with_logs_of(
    node: &Shared,
    address: Address,
    logs: Vec<Value>,
) -> crate::Observed {
    let behind = Behind::new(connect(node).await.rpc_url.clone());
    let state = (behind, address, logs);
    async fn one((behind, address, logs): &(Behind, Address, Vec<Value>), asked: &Value) -> Value {
        if asked["method"] == "eth_getLogs" {
            let wanted = format!("{address:?}").to_lowercase();
            let names = |one: &Value| one.as_str().is_some_and(|one| one.to_lowercase() == wanted);
            let named = match &asked["params"][0]["address"] {
                Value::Array(many) => many.iter().any(names),
                one => names(one),
            };
            if named {
                return json!({"jsonrpc": "2.0", "id": asked["id"], "result": logs});
            }
        }
        behind.forward(asked).await
    }
    let app =
        Router::new()
            .route(
                "/",
                post(
                    |State(state): State<(Behind, Address, Vec<Value>)>,
                     Json(body): Json<Value>| async move {
                        match body {
                            Value::Array(batch) => {
                                let mut answered = Vec::new();
                                for asked in &batch {
                                    answered.push(one(&state, asked).await);
                                }
                                Json(Value::Array(answered))
                            }
                            asked => Json(one(&state, &asked).await),
                        }
                    },
                ),
            )
            .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = ChainConfig {
        label: "stand-in with logs".to_string(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        confirmations: None,
        public_rpc: url,
        logs_rpc: None,
        log_range: None,
    };
    Client::connect(&config, Signing::ReadOnly)
        .await
        .unwrap()
        .into()
}

/// A log `address` wrote of `event`, whose indexed fields are `topics`.
pub fn a_log(address: Address, signature: B256, topics: &[B256], block: u64, index: u64) -> Value {
    let mut all = vec![format!("{signature:?}")];
    all.extend(topics.iter().map(|topic| format!("{topic:?}")));
    json!({
        "address": format!("{address:?}"),
        "topics": all,
        "data": "0x",
        "blockNumber": format!("0x{block:x}"),
        "blockHash": format!("{:?}", B256::repeat_byte(0xcc)),
        "transactionHash": format!("{:?}", B256::repeat_byte(0xdd)),
        "transactionIndex": "0x0",
        "logIndex": format!("0x{index:x}"),
        "removed": false,
    })
}

/// Runs after every transaction the node took, with its target and calldata,
/// so a test can change what the chain answers once a call has landed.
pub type Hook = Arc<dyn Fn(&Shared, Address, &[u8]) + Send + Sync>;

/// A client in front of the stand-in that calls `hook` for each send the
/// node took.
pub async fn connect_with_hook(node: &Shared, hook: Hook) -> crate::Observed {
    let behind = Behind::new(connect(node).await.rpc_url.clone());
    let state = (node.clone(), behind, hook);
    let app = Router::new()
        .route(
            "/",
            post(
                |State((node, behind, hook)): State<(Shared, Behind, Hook)>,
                 Json(body): Json<Value>| async move {
                    let answer: Value = behind.forward(&body).await;
                    let pairs: Vec<(Value, Value)> = match (&body, &answer) {
                        (Value::Array(asked), Value::Array(answered)) => asked
                            .iter()
                            .cloned()
                            .zip(answered.iter().cloned())
                            .collect(),
                        (asked, answered) => vec![(asked.clone(), answered.clone())],
                    };
                    for (asked, answered) in pairs {
                        if asked["method"] == "eth_sendTransaction"
                            && answered.get("error").is_none()
                        {
                            let (to, data) = tx_fields(&asked["params"]);
                            hook(&node, to, &data);
                        }
                    }
                    Json(answer)
                },
            ),
        )
        .with_state(state);
    serve_in_front(app, "stand-in with a hook").await.into()
}

/// Which reads a client fails, by their contract and full calldata.
pub type Failing = Arc<dyn Fn(Address, &[u8]) -> bool + Send + Sync>;

/// A client in front of the stand-in whose reads `failing` picks out answer
/// with an error, as a node that drops one request does; everything else
/// goes to the stand-in. The stand-in answers by selector alone, so this is
/// how one claim or one holder is made unreadable while the rest are not.
pub async fn connect_failing(node: &Shared, failing: Failing) -> crate::Observed {
    let behind = Behind::new(connect(node).await.rpc_url.clone());
    let state = (behind, failing);
    async fn one((behind, failing): &(Behind, Failing), asked: &Value) -> Value {
        if asked["method"] == "eth_call" {
            let (to, data) = tx_fields(&asked["params"]);
            if failing(to, &data) {
                return json!({"jsonrpc": "2.0", "id": asked["id"], "error": {"code": -32000, "message": "the node dropped this read"}});
            }
        }
        behind.forward(asked).await
    }
    let app = Router::new()
        .route(
            "/",
            post(
                |State(state): State<(Behind, Failing)>, Json(body): Json<Value>| async move {
                    match body {
                        Value::Array(batch) => {
                            let mut answered = Vec::new();
                            for asked in &batch {
                                answered.push(one(&state, asked).await);
                            }
                            Json(Value::Array(answered))
                        }
                        asked => Json(one(&state, &asked).await),
                    }
                },
            ),
        )
        .with_state(state);
    serve_in_front(app, "stand-in failing some reads")
        .await
        .into()
}

/// A client in front of the stand-in whose node answers a send of `C` with
/// `refusal` (a JSON-RPC error object) for as long as `turning_away` holds,
/// without the call reaching the stand-in; everything else goes through.
/// This is a node turning a transaction away for its own reasons (the
/// sender can't pay for gas, a rate limit, a fee too low) rather than the
/// contract reverting it.
pub async fn connect_turning_away<C: SolCall>(
    node: &Shared,
    refusal: Value,
    turning_away: Arc<std::sync::atomic::AtomicBool>,
) -> crate::Observed {
    type TurningAway = (Behind, Value, Arc<std::sync::atomic::AtomicBool>);
    let behind = Behind::new(connect(node).await.rpc_url.clone());
    let state: TurningAway = (behind, refusal, turning_away);
    async fn one<C: SolCall>(
        (behind, refusal, turning_away): &TurningAway,
        asked: &Value,
    ) -> Value {
        if asked["method"] == "eth_sendTransaction" && turning_away.load(Ordering::SeqCst) {
            let (_, data) = tx_fields(&asked["params"]);
            if selector_of(&data) == C::SELECTOR {
                return json!({"jsonrpc": "2.0", "id": asked["id"], "error": refusal});
            }
        }
        behind.forward(asked).await
    }
    let app = Router::new()
        .route(
            "/",
            post(
                |State(state): State<TurningAway>, Json(body): Json<Value>| async move {
                    match body {
                        Value::Array(batch) => {
                            let mut answered = Vec::new();
                            for asked in &batch {
                                answered.push(one::<C>(&state, asked).await);
                            }
                            Json(Value::Array(answered))
                        }
                        asked => Json(one::<C>(&state, &asked).await),
                    }
                },
            ),
        )
        .with_state(state);
    serve_in_front(app, "stand-in turning sends away")
        .await
        .into()
}

/// Serve a proxy in front of the stand-in, and connect to it.
async fn serve_in_front(app: Router, label: &str) -> Client {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = ChainConfig {
        label: label.to_string(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        confirmations: None,
        public_rpc: url,
        logs_rpc: None,
        log_range: None,
    };
    Client::connect(&config, Signing::ReadOnly).await.unwrap()
}

/// A client in front of the stand-in whose node, while `reverting` is false,
/// turns every send away for want of gas, and once it is true takes each and
/// mines it reverted.
pub async fn connect_turning_away_then_reverting(
    node: &Shared,
    reverting: Arc<std::sync::atomic::AtomicBool>,
) -> crate::Observed {
    type Reverting = (Behind, Arc<std::sync::atomic::AtomicBool>);
    let behind = Behind::new(connect(node).await.rpc_url.clone());
    async fn one((behind, reverting): &Reverting, asked: &Value) -> Value {
        let reverting = reverting.load(Ordering::SeqCst);
        if asked["method"] == "eth_sendTransaction" && !reverting {
            return json!({"jsonrpc": "2.0", "id": asked["id"], "error": {"code": -32000, "message": "insufficient funds for gas * price + value"}});
        }
        let mut answer: Value = behind.forward(asked).await;
        if asked["method"] == "eth_getTransactionReceipt" && reverting {
            answer["result"]["status"] = json!("0x0");
        }
        answer
    }
    let app = Router::new()
        .route(
            "/",
            post(
                |State(state): State<Reverting>, Json(body): Json<Value>| async move {
                    match body {
                        Value::Array(batch) => {
                            let mut answered = Vec::new();
                            for asked in &batch {
                                answered.push(one(&state, asked).await);
                            }
                            Json(Value::Array(answered))
                        }
                        asked => Json(one(&state, &asked).await),
                    }
                },
            ),
        )
        .with_state((behind, reverting));
    serve_in_front(app, "stand-in turning sends away, then reverting them")
        .await
        .into()
}

/// A client in front of the stand-in whose node, while `reverting` holds,
/// mines every send it takes reverted; otherwise each goes through.
pub async fn connect_reverting(
    node: &Shared,
    reverting: Arc<std::sync::atomic::AtomicBool>,
) -> crate::Observed {
    type Reverting = (Behind, Arc<std::sync::atomic::AtomicBool>);
    let behind = Behind::new(connect(node).await.rpc_url.clone());
    async fn one((behind, reverting): &Reverting, asked: &Value) -> Value {
        let mut answer: Value = behind.forward(asked).await;
        if asked["method"] == "eth_getTransactionReceipt" && reverting.load(Ordering::SeqCst) {
            answer["result"]["status"] = json!("0x0");
        }
        answer
    }
    let app = Router::new()
        .route(
            "/",
            post(
                |State(state): State<Reverting>, Json(body): Json<Value>| async move {
                    match body {
                        Value::Array(batch) => {
                            let mut answered = Vec::new();
                            for asked in &batch {
                                answered.push(one(&state, asked).await);
                            }
                            Json(Value::Array(answered))
                        }
                        asked => Json(one(&state, &asked).await),
                    }
                },
            ),
        )
        .with_state((behind, reverting));
    serve_in_front(app, "stand-in reverting sends").await.into()
}

/// An executor that answers a 404 (it hasn't taken the vault on yet), then
/// its signed total, then a 404 for good (moved where the keeper's URL no
/// longer reaches).
pub async fn executor_serving_a_total_once_between_404s() -> String {
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = Router::new().route(
        "/usage/{chain}/{vault}",
        axum::routing::get(move || {
            let asked = asked.clone();
            async move {
                use axum::response::IntoResponse;
                if asked.fetch_add(1, Ordering::SeqCst) != 1 {
                    return axum::http::StatusCode::NOT_FOUND.into_response();
                }
                Json(json!({
                    "total": "1000",
                    "as_of": 1_700_000_000u64,
                    "signature": format!("0x{}", "11".repeat(65)),
                }))
                .into_response()
            }
        }),
    );
    serve(app).await
}

/// What the keeper logs while a test runs, on this thread.
#[derive(Clone, Default)]
pub struct Logged(Arc<Mutex<Vec<u8>>>);

impl Logged {
    /// Capture this thread's logs until the guard is dropped.
    pub fn capture() -> (Logged, tracing::subscriber::DefaultGuard) {
        let logged = Logged::default();
        let writer = logged.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        (logged, tracing::subscriber::set_default(subscriber))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
    }
}

impl std::io::Write for Logged {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
