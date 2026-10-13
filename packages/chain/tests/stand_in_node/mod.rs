//! A stand-in node for the follower's tests: a local anvil of the test's
//! own (killed when dropped), and a scriptable JSON-RPC proxy in front of
//! it that misbehaves the ways public endpoints do: it answers late, lags a
//! few blocks behind, fails for a while, ignores block pins (answering at
//! its head), refuses state overrides, caps a call's gas without saying so,
//! refuses wide log ranges, or leaves a log out of an answer once.
//!
//! Without anvil on the path the tests that need it say so and fail: the
//! follower's properties are only shown against a real EVM.
//!
//! Each test file uses part of this, and the rest would be dead code to it.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use chain::ChainConfig;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A local anvil, stopped when dropped.
pub struct Anvil {
    child: Child,
    pub url: String,
    pub chain_id: u64,
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl Anvil {
    /// An anvil on a port of its own, with no automatic mining, its genesis
    /// at `timestamp`. Each test uses its own chain id, so the request
    /// counters of one test never mix with another's.
    pub async fn start(chain_id: u64, timestamp: u64) -> Self {
        let port = free_port();
        let child = Command::new("anvil")
            .args([
                "--silent",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--chain-id",
                &chain_id.to_string(),
                "--timestamp",
                &timestamp.to_string(),
                "--no-mining",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("anvil is required for the follower's tests");
        let address: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let stop = std::time::Instant::now() + Duration::from_secs(20);
        while tokio::net::TcpStream::connect(address).await.is_err() {
            assert!(std::time::Instant::now() < stop, "anvil did not start");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let anvil = Self {
            child,
            url: format!("http://{address}"),
            chain_id,
        };
        // Answering, not only listening.
        while anvil.rpc("eth_chainId", json!([])).await.is_err() {
            assert!(std::time::Instant::now() < stop, "anvil did not answer");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        anvil
    }

    /// One JSON-RPC request; its result, or its error object as text.
    pub async fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        rpc(&self.url, method, params).await
    }

    /// Mine `count` blocks, each `interval` seconds after the last.
    pub async fn mine(&self, count: u64, interval: u64) {
        self.rpc(
            "anvil_mine",
            json!([format!("0x{count:x}"), format!("0x{interval:x}")]),
        )
        .await
        .unwrap();
    }

    pub async fn head(&self) -> u64 {
        quantity(&self.rpc("eth_blockNumber", json!([])).await.unwrap())
    }

    /// Block `number`'s header fields: (hash, parent hash, timestamp).
    pub async fn block(&self, number: u64) -> (B256, B256, u64) {
        let block = self
            .rpc(
                "eth_getBlockByNumber",
                json!([format!("0x{number:x}"), false]),
            )
            .await
            .unwrap();
        (
            block["hash"].as_str().unwrap().parse().unwrap(),
            block["parentHash"].as_str().unwrap().parse().unwrap(),
            quantity(&block["timestamp"]),
        )
    }

    pub async fn set_code(&self, at: Address, code: &Bytes) {
        self.rpc("anvil_setCode", json!([at, code])).await.unwrap();
    }

    /// Send a transaction from anvil's first account and mine it alone.
    pub async fn send(&self, to: Address, data: Vec<u8>) {
        let accounts = self.rpc("eth_accounts", json!([])).await.unwrap();
        self.rpc(
            "eth_sendTransaction",
            json!([{"from": accounts[0], "to": to, "data": Bytes::from(data), "gas": "0x1000000"}]),
        )
        .await
        .unwrap();
        self.mine(1, 1).await;
    }

    /// A snapshot of the chain as it is now, to fork from later.
    pub async fn snapshot(&self) -> Value {
        self.rpc("evm_snapshot", json!([])).await.unwrap()
    }

    /// Go back to `snapshot`, dropping every block mined after it, with the
    /// next block stamped seven seconds after the snapshot's head, so every
    /// block mined from here differs from the one it replaces: a fork at the
    /// snapshot's head. (Anvil's own rollback and reorg leave `BLOCKHASH`
    /// answering with the replaced blocks' hashes, which no real node does.)
    pub async fn fork_at(&self, snapshot: &Value) {
        assert_eq!(
            self.rpc("evm_revert", json!([snapshot])).await.unwrap(),
            json!(true)
        );
        let (_, _, timestamp) = self.block(self.head().await).await;
        self.rpc("evm_setNextBlockTimestamp", json!([timestamp + 7]))
            .await
            .unwrap();
    }

    /// Native coin for `to`.
    pub async fn fund(&self, to: Address, wei: U256) {
        self.rpc("anvil_setBalance", json!([to, wei]))
            .await
            .unwrap();
    }
}

pub fn quantity(value: &Value) -> u64 {
    u64::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

/// The runtime code of a test helper contract, from the generated file.
pub fn code(name: &str) -> Bytes {
    let all: BTreeMap<String, String> =
        serde_json::from_str(include_str!("code.json")).expect("the test contract code is JSON");
    alloy::hex::decode(
        all.get(name)
            .unwrap_or_else(|| panic!("no code for {name}")),
    )
    .unwrap()
    .into()
}

pub fn code_hash(name: &str) -> B256 {
    keccak256(code(name))
}

/// A chain config naming `urls` as its endpoints, in order.
pub fn config(urls: &[&str]) -> ChainConfig {
    let rpc = urls.join(" ");
    ChainConfig {
        label: "stand-in".into(),
        rpc: rpc.clone(),
        public_rpc: rpc,
        ws: None,
        dollar: "USDC".into(),
        native_usd: None,
        logs_rpc: None,
        log_range: None,
        confirmations: None,
    }
}

pub async fn rpc(url: &str, method: &str, params: Value) -> Result<Value, String> {
    let answer = post(
        url,
        &json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
    )
    .await
    .map_err(|error| error.to_string())?;
    if let Some(error) = answer.get("error") {
        return Err(error.to_string());
    }
    Ok(answer["result"].clone())
}

async fn post(url: &str, body: &Value) -> Result<Value, String> {
    let client = http_client();
    let response = client
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    response
        .json::<Value>()
        .await
        .map_err(|error| error.to_string())
}

fn http_client() -> alloy::transports::http::reqwest::Client {
    static CLIENT: std::sync::OnceLock<alloy::transports::http::reqwest::Client> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            alloy::transports::http::reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap()
        })
        .clone()
}

/// How the proxy misbehaves; a test changes it while the proxy runs.
#[derive(Clone, Debug, Default)]
pub struct Script {
    /// Hold every answer this long (`eth_chainId` excepted).
    pub lag: Option<Duration>,
    /// Behave as a node this many blocks behind the upstream's head.
    pub behind: u64,
    /// Run a call at the head this many blocks below the head the endpoint
    /// gives everywhere else, as a node behind a load balancer may.
    pub calls_behind: u64,
    /// Fail this many of the next requests (`eth_chainId` excepted), with an
    /// internal error.
    pub fail_next: u32,
    /// Fail every request for these methods.
    pub fail_methods: Vec<String>,
    /// Answer every call and state read at the head, whatever block it was
    /// pinned to (a hash it has never seen included).
    pub ignore_pins: bool,
    /// Refuse any call carrying a state override.
    pub refuse_overrides: bool,
    /// Give a call at most this much gas, without saying so.
    pub gas_cap: Option<u64>,
    /// Refuse a log query wider than this many blocks.
    pub log_range_cap: Option<u64>,
    /// Leave the last log out of the next log answer that has any.
    pub drop_a_log_once: bool,
    /// Refuse an answer larger than this many bytes as too big.
    pub response_cap: Option<usize>,
}

/// What the proxy has seen.
#[derive(Debug, Default)]
pub struct Seen {
    /// Requests by method, as forwarded or answered.
    pub methods: BTreeMap<String, u64>,
    /// The `[from, to]` of every log query.
    pub log_ranges: Vec<(u64, u64)>,
}

impl Seen {
    pub fn count(&self, method: &str) -> u64 {
        self.methods.get(method).copied().unwrap_or(0)
    }
}

pub struct Proxy {
    pub url: String,
    pub script: Arc<Mutex<Script>>,
    pub seen: Arc<Mutex<Seen>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Proxy {
    pub fn script(&self, change: impl FnOnce(&mut Script)) {
        change(&mut self.script.lock().unwrap());
    }

    pub fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen.lock().unwrap()
    }

    pub fn forget_seen(&self) {
        *self.seen.lock().unwrap() = Seen::default();
    }

    /// A proxy in front of `upstream`.
    pub async fn start(upstream: &str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let script = Arc::new(Mutex::new(Script::default()));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let upstream = upstream.to_string();
        let (task_script, task_seen) = (script.clone(), seen.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let (script, seen, upstream) =
                    (task_script.clone(), task_seen.clone(), upstream.clone());
                tokio::spawn(serve(socket, upstream, script, seen));
            }
        });
        Self {
            url,
            script,
            seen,
            task,
        }
    }
}

/// Serve one connection: HTTP/1.1 requests, one JSON-RPC body each.
async fn serve(
    mut socket: tokio::net::TcpStream,
    upstream: String,
    script: Arc<Mutex<Script>>,
    seen: Arc<Mutex<Seen>>,
) {
    let mut buffer = Vec::new();
    loop {
        let (start, size) = loop {
            if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                let size = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buffer.len() >= end + 4 + size {
                    break (end + 4, size);
                }
            }
            let mut part = [0u8; 16_384];
            match socket.read(&mut part).await {
                Ok(0) | Err(_) => return,
                Ok(read) => buffer.extend_from_slice(&part[..read]),
            }
        };
        let body: Value = match serde_json::from_slice(&buffer[start..start + size]) {
            Ok(body) => body,
            Err(_) => return,
        };
        buffer.drain(..start + size);
        let answer = match body {
            Value::Array(requests) => {
                let mut answers = Vec::new();
                for request in requests {
                    answers.push(answer(&request, &upstream, &script, &seen).await);
                }
                Value::Array(answers)
            }
            request => answer(&request, &upstream, &script, &seen).await,
        };
        let bytes = serde_json::to_vec(&answer).unwrap();
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            bytes.len()
        );
        if socket.write_all(reply.as_bytes()).await.is_err()
            || socket.write_all(&bytes).await.is_err()
        {
            return;
        }
    }
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn hex(number: u64) -> Value {
    json!(format!("0x{number:x}"))
}

/// Where a block parameter points: a number, a hash, or the head.
enum Pin {
    Head,
    Number(u64),
    Hash(String),
}

fn pin_of(value: Option<&Value>) -> Pin {
    match value {
        None | Some(Value::Null) => Pin::Head,
        Some(Value::String(tag)) if !tag.starts_with("0x") => Pin::Head,
        Some(Value::String(number)) => Pin::Number(quantity(&json!(number))),
        Some(Value::Object(object)) => {
            if let Some(hash) = object.get("blockHash").and_then(Value::as_str) {
                Pin::Hash(hash.to_string())
            } else if let Some(number) = object.get("blockNumber") {
                Pin::Number(quantity(number))
            } else {
                Pin::Head
            }
        }
        Some(_) => Pin::Head,
    }
}

/// The position of the block parameter for the methods that take one.
fn block_param(method: &str) -> Option<usize> {
    match method {
        "eth_call" => Some(1),
        "eth_getBalance" | "eth_getCode" | "eth_getTransactionCount" | "eth_getStorageAt" => {
            Some(if method == "eth_getStorageAt" { 2 } else { 1 })
        }
        _ => None,
    }
}

async fn answer(
    request: &Value,
    upstream: &str,
    script: &Mutex<Script>,
    seen: &Mutex<Seen>,
) -> Value {
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or_default().to_string();
    let mut params = request["params"].clone();
    if method == "eth_chainId" {
        return forward(upstream, &method, params, &id).await;
    }
    let now = script.lock().unwrap().clone();
    *seen
        .lock()
        .unwrap()
        .methods
        .entry(method.clone())
        .or_default() += 1;
    if let Some(lag) = now.lag {
        tokio::time::sleep(lag).await;
    }
    {
        let mut script = script.lock().unwrap();
        if script.fail_next > 0 {
            script.fail_next -= 1;
            return error(&id, -32603, "internal error");
        }
    }
    if now.fail_methods.contains(&method) {
        return error(&id, -32603, "internal error");
    }
    let head = match rpc(upstream, "eth_blockNumber", json!([])).await {
        Ok(head) => quantity(&head),
        Err(text) => return error(&id, -32603, &text),
    };
    let visible = head.saturating_sub(now.behind);
    match method.as_str() {
        "eth_blockNumber" => return json!({"jsonrpc": "2.0", "id": id, "result": hex(visible)}),
        "eth_getBlockByNumber" => {
            let number = match pin_of(params.get(0)) {
                Pin::Number(number) => number,
                _ => visible,
            };
            if number > visible {
                return json!({"jsonrpc": "2.0", "id": id, "result": null});
            }
            params[0] = hex(number);
        }
        "eth_getLogs" => {
            let filter = &mut params[0];
            let from = filter
                .get("fromBlock")
                .map_or(visible, |value| match pin_of(Some(value)) {
                    Pin::Number(number) => number,
                    _ => visible,
                });
            let to = filter
                .get("toBlock")
                .map_or(visible, |value| match pin_of(Some(value)) {
                    Pin::Number(number) => number,
                    _ => visible,
                });
            seen.lock().unwrap().log_ranges.push((from, to));
            if let Some(cap) = now.log_range_cap
                && to >= from
                && to - from + 1 > cap
            {
                return error(
                    &id,
                    -32614,
                    &format!("eth_getLogs is limited to a {cap} range"),
                );
            }
            // A node behind answers with what it has, and no error.
            filter["toBlock"] = hex(to.min(visible));
            filter["fromBlock"] = hex(from);
            let mut answer = forward(upstream, &method, params, &id).await;
            if let Some(logs) = answer["result"].as_array_mut()
                && !logs.is_empty()
            {
                let mut script = script.lock().unwrap();
                if script.drop_a_log_once {
                    script.drop_a_log_once = false;
                    logs.pop();
                }
            }
            return answer;
        }
        _ => {}
    }
    if let Some(at) = block_param(&method) {
        if method == "eth_call"
            && now.refuse_overrides
            && params.get(2).is_some_and(|o| !o.is_null())
        {
            return error(&id, 12, "Can't route your request to suitable provider");
        }
        if method == "eth_call"
            && let Some(cap) = now.gas_cap
            && let Some(gas) = params[0].get("gas")
            && quantity(gas) > cap
        {
            params[0]["gas"] = hex(cap);
        }
        while params.as_array().is_some_and(|list| list.len() <= at) {
            params.as_array_mut().unwrap().push(Value::Null);
        }
        let target = match pin_of(params.get(at)) {
            _ if now.ignore_pins => hex(visible),
            Pin::Head if method == "eth_call" => hex(visible.saturating_sub(now.calls_behind)),
            Pin::Head => hex(visible),
            Pin::Number(number) if number > visible => {
                return error(&id, -32000, "header not found");
            }
            Pin::Number(number) => hex(number),
            Pin::Hash(hash) => {
                match rpc(upstream, "eth_getBlockByHash", json!([hash, false])).await {
                    Ok(block) if !block.is_null() && quantity(&block["number"]) <= visible => {
                        params[at].clone()
                    }
                    _ => return error(&id, -32000, "header not found"),
                }
            }
        };
        params[at] = target;
    }
    let answer = forward(upstream, &method, params, &id).await;
    if let Some(cap) = now.response_cap
        && serde_json::to_vec(&answer).map_or(0, |bytes| bytes.len()) > cap
    {
        return error(&id, -32000, "Response is too big");
    }
    answer
}

async fn forward(upstream: &str, method: &str, params: Value, id: &Value) -> Value {
    match post(
        upstream,
        &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
    )
    .await
    {
        Ok(answer) => answer,
        Err(text) => error(id, -32603, &text),
    }
}

alloy_sol_types::sol!(
    #[sol(all_derives)]
    "../../contracts/evm/test/follow/FollowProbe.sol"
);

/// Where the tests put the probe contract, and a second copy of it.
pub const PROBE: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf0, 0x11, 0, 1,
]);
pub const PROBE_TWO: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf0, 0x11, 0, 2,
]);
/// Where the tests put the stand-in Multicall3.
pub const MULTICALL: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xca, 0x11, 0, 3,
]);

/// The checked-in defaults, with a read budget roomy enough for a busy
/// test machine.
pub fn policy() -> chain::rpc_policy::RpcPolicy {
    chain::rpc_policy::RpcPolicy {
        read_budget_ms: 10_000,
        ..Default::default()
    }
}

pub async fn connect(urls: &[&str]) -> chain::Client {
    chain::Client::connect(&config(urls), chain::Signing::ReadOnly)
        .await
        .expect("the stand-in endpoints answer")
}

/// A follower of `urls` with `policy`, Multicall3 looked for where the
/// tests put the stand-in.
pub async fn follower(
    urls: &[&str],
    policy: chain::rpc_policy::RpcPolicy,
) -> chain::follow::Follower {
    chain::follow::Follower::with_options(
        connect(urls).await,
        policy,
        chain::follow::Options {
            multicall3: MULTICALL,
            multicall3_code_hash: code_hash("StandInMulticall3"),
        },
    )
}

/// The probe at `at`, its value set to `value`.
pub async fn put_probe(anvil: &Anvil, at: Address, value: u64) {
    use alloy_sol_types::SolCall;
    anvil.set_code(at, &code("FollowProbe")).await;
    anvil
        .send(
            at,
            FollowProbe::setCall {
                next: U256::from(value),
            }
            .abi_encode(),
        )
        .await;
}
