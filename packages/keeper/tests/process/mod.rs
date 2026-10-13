//! The keeper run as a process against scripted nodes, as an operator runs
//! it: what it sent, what it logged and what its `/health` answered.
//!
//! A node answers `eth_call` by contract and full calldata first, then by
//! contract and selector; anything unscripted reverts (code 3), which the
//! keeper takes as a read that failed. A call with no contract (creation
//! code run by `eth_call`, as the keeper simulates an unwind and what it
//! leaves) is asked of the hooks as one to the zero address. The keeper's
//! check, as a chain connects, that its node simulates (that probe, of a
//! station at [`NOTHING_AT`]) is answered as a real node answers it, before
//! the hooks, unless a request hook refuses it. A call run at a set block
//! time (a send simulated at the moment it would land) goes through; the
//! send hook judges the send itself. Every address
//! has code but those a test marks empty, and a call to one of those returns
//! nothing, as a real chain's does. Every transaction it takes is mined at
//! once with a successful receipt unless a test's hook turns it away.
//!
//! Each test file uses part of this, and the rest would be dead code to it.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::Read as _;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, FixedBytes, U256, keccak256};
use alloy::sol_types::SolCall;
use axum::Json;
use axum::extract::State;
use axum::routing::post;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::StationSpec;
use chain::bindings::vault::{Tank, Timers};
use chain::{Deployments, Roles};
use serde_json::{Value, json};

alloy::sol! {
    interface IReplacesFactory {
        function previous() external view returns (address);
    }
}

pub const DAY: u64 = 86_400;
pub const WAD: u64 = 1_000_000_000_000_000_000;
pub const FACTORY: Address = Address::repeat_byte(0x66);
pub const VAULT: Address = Address::repeat_byte(0x11);
pub const STATION: Address = Address::repeat_byte(0x22);
pub const KEEPER: Address = Address::repeat_byte(5);
/// Where the keeper's check that the node simulates says the station is:
/// nothing lives there.
pub const NOTHING_AT: Address = Address::new([
    0x5e, 0x1f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x02,
]);

// The scripted node ---------------------------------------------------------

pub type Reply = Result<Vec<u8>, String>;
/// Runs on every `eth_call`; an answer it gives wins over the tables.
pub type CallHook = Arc<dyn Fn(&mut Node, Address, &[u8]) -> Option<Reply> + Send + Sync>;
/// Runs on every `eth_sendTransaction`; an error object it gives turns the
/// send away, otherwise the send is mined.
pub type SendHook = Arc<dyn Fn(&mut Node, Address, &[u8]) -> Option<Value> + Send + Sync>;
/// Runs on every request, by its method and params, before anything else;
/// an error object it gives is the node's answer.
pub type RequestHook = Arc<dyn Fn(&str, &Value) -> Option<Value> + Send + Sync>;

pub struct Node {
    pub chain_id: u64,
    /// How many accounts the node signs for: the keeper's is the fifth.
    pub accounts: u8,
    pub timestamp: u64,
    pub head: u64,
    pub exact: HashMap<(Address, Vec<u8>), Reply>,
    pub by_selector: HashMap<(Address, [u8; 4]), Reply>,
    /// Every transaction mined, in order.
    pub sent: Vec<(Address, Vec<u8>)>,
    /// Every `eth_call` asked, by selector.
    pub calls: Vec<(Address, [u8; 4])>,
    pub vars: HashMap<&'static str, u64>,
    pub on_call: Option<CallHook>,
    pub on_send: Option<SendHook>,
    pub on_request: Option<RequestHook>,
    /// Addresses with nothing deployed.
    pub empty: Vec<Address>,
}

pub type Shared = Arc<Mutex<Node>>;

pub fn node(timestamp: u64) -> Shared {
    Arc::new(Mutex::new(Node {
        chain_id: 31337,
        accounts: 10,
        timestamp,
        head: 100,
        exact: HashMap::new(),
        by_selector: HashMap::new(),
        sent: Vec::new(),
        calls: Vec::new(),
        vars: HashMap::new(),
        on_call: None,
        on_send: None,
        on_request: None,
        empty: Vec::new(),
    }))
}

pub fn answers<C: SolCall>(node: &Shared, to: Address, _call: C, ret: C::Return) {
    node.lock()
        .unwrap()
        .by_selector
        .insert((to, C::SELECTOR), Ok(C::abi_encode_returns(&ret)));
}

pub fn answers_exactly<C: SolCall>(node: &Shared, to: Address, call: C, ret: C::Return) {
    node.lock()
        .unwrap()
        .exact
        .insert((to, call.abi_encode()), Ok(C::abi_encode_returns(&ret)));
}

pub fn sent<C: SolCall>(node: &Shared, to: Address) -> usize {
    node.lock()
        .unwrap()
        .sent
        .iter()
        .filter(|(at, data)| *at == to && data.len() >= 4 && data[..4] == C::SELECTOR)
        .count()
}

pub fn called<C: SolCall>(node: &Shared, to: Address) -> usize {
    node.lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(at, selector)| *at == to && *selector == C::SELECTOR)
        .count()
}

fn tx_fields(tx: &Value) -> (Address, Vec<u8>) {
    let to: Address = tx["to"]
        .as_str()
        .and_then(|to| to.parse().ok())
        .unwrap_or(Address::ZERO);
    let data = tx
        .get("input")
        .or_else(|| tx.get("data"))
        .and_then(Value::as_str)
        .unwrap_or("0x");
    (to, alloy::hex::decode(data).unwrap())
}

fn answer(node: &Shared, asked: &Value) -> Value {
    let id = asked["id"].clone();
    let params = &asked["params"];
    let mut node = node.lock().unwrap();
    let method = asked["method"].as_str().unwrap_or_default();
    if let Some(refusal) = node
        .on_request
        .clone()
        .and_then(|hook| hook(method, params))
    {
        return json!({"jsonrpc": "2.0", "id": id, "error": refusal});
    }
    let result: Result<Value, Value> = match method {
        "eth_chainId" => Ok(json!(format!("0x{:x}", node.chain_id))),
        "eth_accounts" => Ok(json!(
            (1u8..=node.accounts)
                .map(|n| format!("{:?}", Address::repeat_byte(n)))
                .collect::<Vec<_>>()
        )),
        "eth_blockNumber" => Ok(json!(format!("0x{:x}", node.head))),
        "eth_getBlockByNumber" => {
            let number = params[0]
                .as_str()
                .and_then(|asked| asked.strip_prefix("0x"))
                .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                .unwrap_or(node.head)
                .min(node.head);
            let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
            block.header.inner.number = number;
            // Every block carries the head's time: the keeper measures a
            // block interval of one second.
            block.header.inner.timestamp = node.timestamp;
            block.header.inner.base_fee_per_gas = Some(1_000_000_000);
            Ok(serde_json::to_value(block).unwrap())
        }
        "eth_gasPrice" | "eth_maxPriorityFeePerGas" => Ok(json!("0x3b9aca00")),
        "eth_estimateGas" => Ok(json!("0x5208")),
        "eth_getLogs" => Ok(json!([])),
        "eth_getCode" => {
            let at: Address = params[0]
                .as_str()
                .and_then(|at| at.parse().ok())
                .unwrap_or_default();
            Ok(json!(if node.empty.contains(&at) {
                "0x"
            } else {
                "0x00"
            }))
        }
        // The keeper's check that the node simulates: the calls its probe
        // makes, to an address nothing lives at, return nothing.
        "eth_call" if simulation_check(params) => Ok(json!(alloy::hex::encode_prefixed([0u8; 96]))),
        // A send run at the moment it would land (the block's time set) goes
        // through: whether the send itself is taken is the send hook's to say.
        "eth_call" if params.get(3).is_some_and(Value::is_object) => Ok(json!("0x")),
        "eth_call" => {
            let (to, data) = tx_fields(&params[0]);
            let selector: [u8; 4] = data
                .get(..4)
                .and_then(|four| four.try_into().ok())
                .unwrap_or_default();
            node.calls.push((to, selector));
            let hooked = node
                .on_call
                .clone()
                .and_then(|hook| hook(&mut node, to, &data));
            let reply = hooked
                .or_else(|| node.empty.contains(&to).then(|| Ok(Vec::new())))
                .or_else(|| node.exact.get(&(to, data.clone())).cloned())
                .or_else(|| node.by_selector.get(&(to, selector)).cloned());
            match reply {
                Some(Ok(bytes)) => Ok(json!(alloy::hex::encode_prefixed(bytes))),
                Some(Err(reason)) => {
                    Err(json!({"code": 3, "message": format!("execution reverted: {reason}")}))
                }
                None => Err(json!({"code": 3, "message": "execution reverted: unscripted"})),
            }
        }
        "eth_sendTransaction" => {
            let (to, data) = tx_fields(&params[0]);
            let turned = node
                .on_send
                .clone()
                .and_then(|hook| hook(&mut node, to, &data));
            match turned {
                Some(error) => Err(error),
                None => {
                    let hash =
                        keccak256([&(node.sent.len() as u64).to_be_bytes()[..], &data].concat());
                    node.sent.push((to, data));
                    Ok(json!(hash))
                }
            }
        }
        "eth_getTransactionReceipt" => Ok(json!({
            "transactionHash": params[0],
            "transactionIndex": "0x0",
            "blockHash": B256::repeat_byte(0xbb),
            "blockNumber": format!("0x{:x}", node.head),
            "from": KEEPER,
            "to": STATION,
            "cumulativeGasUsed": "0x5208",
            "gasUsed": "0x5208",
            "effectiveGasPrice": "0x3b9aca00",
            "contractAddress": null,
            "logs": [],
            "logsBloom": format!("0x{}", "00".repeat(256)),
            "status": "0x1",
            "type": "0x2",
        })),
        other => Err(json!({"code": -32601, "message": format!("not here: {other}")})),
    };
    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    }
}

/// Whether a call is the keeper's check that the node simulates: its probe,
/// as creation code or as an overridden contract's code, names
/// [`NOTHING_AT`].
pub fn simulation_check(params: &Value) -> bool {
    let names = |code: &[u8]| code.windows(20).any(|at| at == NOTHING_AT.as_slice());
    let created = params[0].get("to").is_none_or(Value::is_null) && names(&tx_fields(&params[0]).1);
    let overridden = params
        .get(2)
        .and_then(Value::as_object)
        .is_some_and(|overrides| {
            overrides.values().any(|account| {
                account["code"]
                    .as_str()
                    .and_then(|hex| alloy::hex::decode(hex).ok())
                    .is_some_and(|code| names(&code))
            })
        });
    created || overridden
}

pub async fn serve_node(node: Shared) -> String {
    let app = axum::Router::new()
        .route(
            "/",
            post(
                |State(node): State<Shared>, Json(body): Json<Value>| async move {
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
    serve(app).await
}

pub async fn serve(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    url
}

// The keeper, as a process ---------------------------------------------------

pub struct Keeper {
    child: Child,
    port: u16,
    log: std::path::PathBuf,
    dir: std::path::PathBuf,
}

impl Drop for Keeper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Keeper {
    pub fn logged(&self) -> String {
        let mut text = String::new();
        if let Ok(mut file) = std::fs::File::open(&self.log) {
            let _ = file.read_to_string(&mut text);
        }
        text
    }

    pub async fn health(&self) -> Option<(u16, String)> {
        let reply = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{}/health", self.port))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .ok()?;
        let status = reply.status().as_u16();
        Some((status, reply.text().await.unwrap_or_default()))
    }

    /// Ask its health until `wanted` holds of the status, for at most
    /// `limit`; the last answer.
    /// Ask its health until its answer says `text`, for at most `limit`;
    /// the last answer. A keeper just started answers 503 while its chains
    /// connect, which says nothing of the trouble a test waits for.
    pub async fn health_saying(&self, limit: Duration, text: &str) -> Option<(u16, String)> {
        let started = Instant::now();
        loop {
            let health = self.health().await;
            if health.as_ref().is_some_and(|(_, said)| said.contains(text))
                || started.elapsed() >= limit
            {
                return health;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub async fn health_until(
        &self,
        limit: Duration,
        wanted: impl Fn(u16) -> bool,
    ) -> Option<(u16, String)> {
        let started = Instant::now();
        loop {
            let health = self.health().await;
            if health.as_ref().is_some_and(|(status, _)| wanted(*status))
                || started.elapsed() >= limit
            {
                return health;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    pub fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

pub fn deployments(chain_id: u64) -> Deployments {
    let nobody = Address::repeat_byte(0x55);
    Deployments {
        chain_key: format!("evm:{chain_id}"),
        chain_id,
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
        factory: FACTORY,
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

/// Start the keeper serving Base alone at `rpc`, asking `executor` for
/// totals, with its health on a port of its own.
pub fn start_keeper(test: &str, rpc: &str, executor: &str) -> Keeper {
    start_keeper_on(test, &[("base", 31337, rpc)], executor)
}

/// [`start_keeper`], with a halted vault's close held for its bill, and a
/// closed vault's last bill waited for, `seconds` on the keeper's own clock
/// rather than fifteen minutes: the chain's clock, which the test moves,
/// is still waited on in full.
pub fn start_keeper_holding(test: &str, rpc: &str, executor: &str, seconds: u64) -> Keeper {
    let seconds = seconds.to_string();
    start(
        test,
        &[("base", 31337, rpc)],
        executor,
        &["--own-clock-hold", &seconds],
    )
}

/// [`start_keeper`], with an executor that answers 404 for every vault, and
/// serves none, said in its health after `seconds` rather than an hour.
pub fn start_keeper_serving_none_for(
    test: &str,
    rpc: &str,
    executor: &str,
    seconds: u64,
) -> Keeper {
    let seconds = seconds.to_string();
    start(
        test,
        &[("base", 31337, rpc)],
        executor,
        &["--serving-none-for", &seconds],
    )
}

/// Start the keeper serving each of `chains` (a vault chain, its chain id,
/// its node), asking `executor` for totals, with its health on a port of its
/// own. Serving more than one, it serves every vault chain of the network:
/// those not given, Solana's among them, have a node out of reach.
pub fn start_keeper_on(test: &str, chains: &[(&str, u64, &str)], executor: &str) -> Keeper {
    start(test, chains, executor, &[])
}

/// Start one chain with `settings` overriding its RPC settings in the file
/// the process loads.
pub fn start_keeper_with_rpc_policy(
    test: &str,
    rpc: &str,
    executor: &str,
    settings: &[(&str, u64)],
) -> Keeper {
    start_configured(test, &[("base", 31337, rpc)], executor, &[], settings)
}

/// Start each of `chains` with `args` on the command line and `settings`
/// overriding every chain's RPC settings in the file the process loads.
pub fn start_keeper_configured(
    test: &str,
    chains: &[(&str, u64, &str)],
    executor: &str,
    args: &[&str],
    settings: &[(&str, u64)],
) -> Keeper {
    start_configured(test, chains, executor, args, settings)
}

fn start(test: &str, chains: &[(&str, u64, &str)], executor: &str, shortened: &[&str]) -> Keeper {
    start_configured(test, chains, executor, shortened, &[])
}

fn start_configured(
    test: &str,
    chains: &[(&str, u64, &str)],
    executor: &str,
    shortened: &[&str],
    settings: &[(&str, u64)],
) -> Keeper {
    let dir = std::env::temp_dir().join(format!("keeper-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut network = Vec::new();
    for (vault_chain, chain_id, rpc) in chains {
        network.push((
            vault_chain.to_string(),
            format!("evm:{chain_id}"),
            rpc.to_string(),
        ));
    }
    if chains.len() > 1 {
        for (vault_chain, key) in [
            ("base", "evm:31347"),
            ("hyperliquid", "evm:31348"),
            ("robinhood", "evm:31349"),
            ("solana", "solana:localnet"),
        ] {
            if !chains.iter().any(|(given, ..)| *given == vault_chain) {
                let nowhere = "http://127.0.0.1:1".to_string();
                network.push((vault_chain.to_string(), key.to_string(), nowhere));
            }
        }
    }
    let mut toml = String::new();
    for (_, key, rpc) in &network {
        toml.push_str(&format!(
            "[chains.\"{key}\"]\nlabel = \"stand-in {key}\"\nrpc = \"{rpc}\"\ndollar = \"USDC\"\n\n"
        ));
    }
    toml.push_str("[networks.local]\n");
    for (vault_chain, key, _) in &network {
        toml.push_str(&format!("{vault_chain} = \"{key}\"\n"));
    }
    if !settings.is_empty() {
        for (_, key, _) in &network {
            toml.push_str(&format!("\n[rpc.chains.\"{key}\"]\n"));
            for (name, value) in settings {
                toml.push_str(&format!("{name} = {value}\n"));
            }
        }
    }
    let config = dir.join("chains.toml");
    std::fs::write(&config, toml).unwrap();
    let all: Vec<Deployments> = chains
        .iter()
        .map(|(_, chain_id, _)| deployments(*chain_id))
        .collect();
    let log = dir.join("keeper.log");
    let port = free_port();
    let mut command = Command::new(env!("CARGO_BIN_EXE_keeper"));
    command
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUST_LOG", std::env::var("RUST_LOG").unwrap_or_default())
        .env(
            "SHONIN_DEPLOYMENTS_JSON",
            serde_json::to_string(&all).unwrap(),
        )
        .args(["--network", "local"]);
    if let [(vault_chain, ..)] = chains {
        command.args(["--vault-chain", vault_chain]);
    }
    command.args(shortened);
    let child = command
        .arg("--chains")
        .arg(&config)
        .arg("--deployments-dir")
        .arg(&dir)
        .arg("--store")
        .arg(&dir)
        .args([
            "--api-url",
            "http://127.0.0.1:1",
            "--executor-url",
            executor,
        ])
        .args(["--port", &port.to_string()])
        .stdout(std::fs::File::create(&log).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Keeper {
        child,
        port,
        log,
        dir,
    }
}

pub async fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < limit {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    done()
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

pub fn a_spec(drivers: Vec<B256>) -> StationSpec {
    StationSpec {
        vaultChain: U256::from(31337u64),
        vault: VAULT,
        bundleHash: B256::ZERO,
        drivers,
        bridges: vec![],
        chains: vec![],
        exitBandBps: 100,
    }
}

pub fn a_tank(balance: u64) -> Tank {
    Tank {
        balance: U256::from(balance),
        keepAt: U256::ZERO,
        usageSettled: U256::ZERO,
        usageAsOf: 0,
        lastCollection: 0,
        escapeRequestedAt: 0,
        capCarried: U256::ZERO,
        fillPending: false,
    }
}

/// The factory lists the one vault, and replaces none.
pub fn one_vault(node: &Shared) {
    answers(node, FACTORY, IVaultFactory::vaultsCall {}, vec![VAULT]);
    answers(
        node,
        FACTORY,
        IReplacesFactory::previousCall {},
        Address::ZERO,
    );
}
