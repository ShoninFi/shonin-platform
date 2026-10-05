//! A stand-in node and reviewer for black-box tests of the builder binary.
//!
//! The node holds enough of the registry to bless the builder's key and to
//! say which strategies are registered, and payments as logs; the reviewer
//! notes every review it is asked for and refuses each one, so nothing is
//! compiled, unless a test binary has it clear what it reads ([`CLEARS`])
//! and puts a stand-in `cargo` on the path.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, U256};
use alloy::sol_types::{SolCall, SolEvent};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use chain::bindings::attestation::{IAttestationRegistry, ImageKind, KeyInfo};
use chain::bindings::publishing::IPublishing;
use chain::bindings::strategies::IStrategyRegistry;
use serde_json::{Value, json};
use shared::model::registry::{Job, JobState};

/// The chain's clock: every block is stamped now, by the wall clock.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
pub const PUBLISHING: Address = Address::repeat_byte(0x9b);
const BUILDER_KEY: Address = Address::repeat_byte(0xb1);

/// The stand-in node.
pub struct Node {
    pub head: AtomicU64,
    /// The widest block range one `eth_getLogs` may ask for, as a paid
    /// provider caps it; `None` for no cap.
    pub log_range_cap: Option<u64>,
    /// Whether every `eth_getLogs` fails, as a node that is down does.
    pub logs_fail: AtomicBool,
    /// `Submitted` payments, at their block.
    pub payments: Mutex<Vec<(u64, alloy::rpc::types::Log)>>,
    /// Every `eth_getLogs` range asked for.
    pub ranges: Mutex<Vec<(u64, u64)>>,
    /// Strategy ids the registry holds as registered, each with the bundle
    /// [`REGISTERED_BUNDLE`].
    pub registered: Mutex<Vec<u64>>,
    /// Strategy ids whose registration, sent before the builder started,
    /// lands just after the registry was first asked about them: held as
    /// registered from then on.
    pub landing: Mutex<Vec<u64>>,
    /// How many blocks deep the chain's config says a payment must be;
    /// `None` leaves it unsaid.
    pub confirmations: Option<u64>,
    /// If set, `registrationDigest` answers this, whatever it is asked: a
    /// node that lies.
    pub lying_digest: Mutex<Option<B256>>,
    /// Every digest the node was asked to sign (`eth_sign`).
    pub signed: Mutex<Vec<B256>>,
}

/// The bundle the stand-in registry holds for every registered strategy.
pub const REGISTERED_BUNDLE: B256 = B256::repeat_byte(0x42);

/// The files the store holds for a bundle the builder made and stored, as
/// [`spawn`] takes them.
pub fn stored(bundle: B256) -> Vec<(&'static str, String)> {
    ["component.wasm", "bundle.tar", "hashes.json"]
        .into_iter()
        .map(|name| {
            let path = format!("sha256-{}/{name}", alloy::hex::encode(bundle));
            (&*Box::leak(path.into_boxed_str()), "{}".to_string())
        })
        .collect()
}

/// A node at `head`, whose config has payments read at the head.
pub fn node(head: u64, log_range_cap: Option<u64>) -> Node {
    Node {
        head: head.into(),
        log_range_cap,
        logs_fail: AtomicBool::new(false),
        payments: Mutex::new(Vec::new()),
        ranges: Mutex::new(Vec::new()),
        registered: Mutex::new(Vec::new()),
        landing: Mutex::new(Vec::new()),
        confirmations: Some(0),
        lying_digest: Mutex::new(None),
        signed: Mutex::new(Vec::new()),
    }
}

impl Node {
    /// A payment of the $1 by `author` against `upload`, at `block`.
    pub fn pay(&self, block: u64, strategy_id: u64, author: Address, upload: B256) {
        let event = IPublishing::Submitted {
            strategyId: U256::from(strategy_id),
            author,
            upload,
            fee: U256::from(1_000_000u64),
        };
        let log = alloy::rpc::types::Log {
            inner: alloy::primitives::Log {
                address: PUBLISHING,
                data: event.encode_log_data(),
            },
            block_hash: Some(B256::repeat_byte(0x55)),
            block_number: Some(block),
            block_timestamp: None,
            transaction_hash: Some(B256::repeat_byte(0x56)),
            transaction_index: Some(0),
            log_index: Some(0),
            removed: false,
        };
        self.payments.lock().unwrap().push((block, log));
    }
}

fn hex_u64(value: &Value) -> Option<u64> {
    u64::from_str_radix(value.as_str()?.trim_start_matches("0x"), 16).ok()
}

fn receipt(hash: &str) -> Value {
    json!({
        "transactionHash": hash,
        "transactionIndex": "0x0",
        "blockHash": format!("0x{}", "22".repeat(32)),
        "blockNumber": "0x1",
        "gasUsed": "0x5208",
        "effectiveGasPrice": "0x1",
        "cumulativeGasUsed": "0x5208",
        "from": format!("{BUILDER_KEY:?}"),
        "to": format!("{:?}", Address::ZERO),
        "contractAddress": null,
        "logs": [],
        "logsBloom": format!("0x{}", "00".repeat(256)),
        "status": "0x1",
        "type": "0x2",
    })
}

async fn rpc(State(node): State<Arc<Node>>, Json(request): Json<Value>) -> Response {
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or_default();
    let error = |code: i64, message: &str| {
        Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
            .into_response()
    };
    let head = node.head.load(Ordering::SeqCst);
    let result = match method {
        "eth_chainId" => json!("0x7a69"),
        "eth_accounts" => json!(
            (1..=10u8)
                .map(|i| format!("{:?}", Address::repeat_byte(i)))
                .collect::<Vec<_>>()
        ),
        "eth_blockNumber" => json!(format!("0x{head:x}")),
        "eth_getBlockByNumber" => {
            let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
            block.header.inner.number = head;
            block.header.inner.timestamp = now();
            block.header.inner.base_fee_per_gas = Some(1_000_000_000);
            serde_json::to_value(block).unwrap()
        }
        "eth_getCode" => json!("0x01"),
        "eth_sign" => {
            if let Some(digest) = request["params"][1]
                .as_str()
                .and_then(|data| alloy::hex::decode(data).ok())
                .filter(|data| data.len() == 32)
            {
                node.signed.lock().unwrap().push(B256::from_slice(&digest));
            }
            json!(format!("0x{}{}", "11".repeat(64), "1b"))
        }
        "eth_sendTransaction" => {
            // The registry refuses a strategy it holds already.
            let tx = &request["params"][0];
            let data = tx["input"]
                .as_str()
                .or_else(|| tx["data"].as_str())
                .unwrap_or("0x");
            let input = alloy::hex::decode(data).unwrap_or_default();
            if let Ok(call) = IStrategyRegistry::registerCall::abi_decode(&input) {
                let id: u64 = call.strategyId.try_into().unwrap();
                if node.registered.lock().unwrap().contains(&id) {
                    return error(3, "execution reverted: AlreadyRegistered");
                }
            }
            json!(format!("0x{}", "33".repeat(32)))
        }
        "eth_getTransactionReceipt" => receipt(request["params"][0].as_str().unwrap_or_default()),
        "eth_getLogs" => {
            let filter = &request["params"][0];
            let from = hex_u64(&filter["fromBlock"]).unwrap_or(0);
            let to = hex_u64(&filter["toBlock"]).unwrap_or(head);
            node.ranges.lock().unwrap().push((from, to));
            if node.logs_fail.load(Ordering::SeqCst) {
                return error(-32603, "the node is unavailable");
            }
            if node
                .log_range_cap
                .is_some_and(|cap| to.saturating_sub(from) + 1 > cap)
            {
                return error(-32005, "query exceeds max block range");
            }
            let logs: Vec<Value> = node
                .payments
                .lock()
                .unwrap()
                .iter()
                .filter(|(block, _)| (from..=to).contains(block))
                .map(|(_, log)| serde_json::to_value(log).unwrap())
                .collect();
            json!(logs)
        }
        "eth_call" => {
            let tx = &request["params"][0];
            let data = tx["input"]
                .as_str()
                .or_else(|| tx["data"].as_str())
                .unwrap_or("0x");
            let input = alloy::hex::decode(data).unwrap_or_default();
            let selector: [u8; 4] = input
                .get(..4)
                .and_then(|s| s.try_into().ok())
                .unwrap_or([0; 4]);
            let out = match selector {
                IStrategyRegistry::isRegisteredCall::SELECTOR => {
                    let call = IStrategyRegistry::isRegisteredCall::abi_decode(&input).unwrap();
                    let id: u64 = call.strategyId.try_into().unwrap();
                    IStrategyRegistry::isRegisteredCall::abi_encode_returns(
                        &node.registered.lock().unwrap().contains(&id),
                    )
                }
                IStrategyRegistry::bundleHashOfCall::SELECTOR => {
                    let call = IStrategyRegistry::bundleHashOfCall::abi_decode(&input).unwrap();
                    let id: u64 = call.strategyId.try_into().unwrap();
                    let mut held = node.registered.lock().unwrap();
                    let registered = held.contains(&id);
                    let mut landing = node.landing.lock().unwrap();
                    if let Some(at) = landing.iter().position(|landed| *landed == id) {
                        landing.remove(at);
                        held.push(id);
                    }
                    drop((held, landing));
                    IStrategyRegistry::bundleHashOfCall::abi_encode_returns(&if registered {
                        REGISTERED_BUNDLE
                    } else {
                        B256::ZERO
                    })
                }
                IStrategyRegistry::registrationDigestCall::SELECTOR => {
                    let call =
                        IStrategyRegistry::registrationDigestCall::abi_decode(&input).unwrap();
                    let registry: Address = tx["to"].as_str().unwrap().parse().unwrap();
                    let digest = node.lying_digest.lock().unwrap().unwrap_or_else(|| {
                        chain::digest::registration(
                            31337,
                            registry,
                            &chain::digest::Registration {
                                strategy_id: call.strategyId,
                                author: call.author,
                                source_hash: call.sourceHash,
                                bundle_hash: call.bundleHash,
                                manifest_hash: call.manifestHash,
                                chains: &call.chains,
                                bridges: &call.bridges,
                                restricted: &call.restricted,
                            },
                        )
                    });
                    IStrategyRegistry::registrationDigestCall::abi_encode_returns(&digest)
                }
                IAttestationRegistry::isImageAllowedCall::SELECTOR => {
                    IAttestationRegistry::isImageAllowedCall::abi_encode_returns(&true)
                }
                IAttestationRegistry::isBlessedAsCall::SELECTOR => {
                    IAttestationRegistry::isBlessedAsCall::abi_encode_returns(&true)
                }
                IAttestationRegistry::blessingTtlCall::SELECTOR => {
                    U256::from(72 * 3600u64).to_be_bytes::<32>().to_vec()
                }
                IAttestationRegistry::keyInfoCall::SELECTOR => {
                    IAttestationRegistry::keyInfoCall::abi_encode_returns(&KeyInfo {
                        imageDigest: B256::ZERO,
                        projectId: B256::ZERO,
                        kind: ImageKind::Builder,
                        strategyHash: B256::ZERO,
                        vaultChain: U256::ZERO,
                        vault: Address::ZERO,
                        trustRoot: B256::ZERO,
                        verifier: Address::ZERO,
                        blessedUntil: now() + 72 * 3600,
                    })
                }
                _ => return error(3, "execution reverted"),
            };
            json!(alloy::hex::encode_prefixed(out))
        }
        _ => return error(-32601, "method not found"),
    };
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

/// Whether the stand-in reviewer clears what it reads: set it before
/// [`spawn`] in a test binary of its own.
pub static CLEARS: AtomicBool = AtomicBool::new(false);

/// Whether the stand-in reviewer is away (restarting, say): while set, every
/// request to it is dropped without an answer.
pub static AWAY: AtomicBool = AtomicBool::new(false);

/// Whether the stand-in reviewer answers 503, as a reviewer not yet connected
/// to the job's chain does.
pub static UNAVAILABLE: AtomicBool = AtomicBool::new(false);

/// Whether the stand-in reviewer answers 500: an error of its own, not one
/// of the statuses a proxy in front of a reviewer that is away answers with,
/// and not a verdict whatever its body says.
pub static ERRORS: AtomicBool = AtomicBool::new(false);

/// Whether the stand-in reviewer answers success with a body that is not a
/// verdict.
pub static GARBLED: AtomicBool = AtomicBool::new(false);

/// How many requests the stand-in reviewer has been sent, answered or not.
pub static ASKED: AtomicU64 = AtomicU64::new(0);

/// The stand-in reviewer: notes the strategy id of every review asked for and
/// refuses it, or clears it when [`CLEARS`] is set.
async fn review(
    State(seen): State<Arc<Mutex<Vec<u64>>>>,
    Json(request): Json<shared::ReviewRequest>,
) -> Response {
    ASKED.fetch_add(1, Ordering::SeqCst);
    if AWAY.load(Ordering::SeqCst) {
        // Dropping the connection unanswered, as a reviewer mid-restart does.
        panic!("the stand-in reviewer is away");
    }
    if UNAVAILABLE.load(Ordering::SeqCst) {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "base is not connected yet: ask again shortly",
        )
            .into_response();
    }
    if ERRORS.load(Ordering::SeqCst) {
        // With a body that reads as a refusal, as an error page might: an
        // answer that is not a success is never a verdict, whatever it says.
        let archive = shared::base64::decode(&request.archive_base64).unwrap();
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(shared::ReviewAnswer {
                source_hash: shared::manifest::hash_bytes(&archive),
                report_hash: "sha256:00".to_string(),
                clearance: None,
            }),
        )
            .into_response();
    }
    if GARBLED.load(Ordering::SeqCst) {
        return (axum::http::StatusCode::OK, "{\"source_hash\": ").into_response();
    }
    seen.lock().unwrap().push(request.strategy_id);
    if CLEARS.load(Ordering::SeqCst) {
        let archive = shared::base64::decode(&request.archive_base64).unwrap();
        return Json(shared::ReviewAnswer {
            source_hash: shared::manifest::hash_bytes(&archive),
            report_hash: "sha256:00".to_string(),
            clearance: Some(format!("0x{}{}", "11".repeat(64), "1b")),
        })
        .into_response();
    }
    Json(
        serde_json::from_value::<shared::ReviewAnswer>(json!({
            "source_hash": "sha256:00",
            "report_hash": "sha256:00",
            "clearance": null,
        }))
        .unwrap(),
    )
    .into_response()
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

static SCRATCH: AtomicU64 = AtomicU64::new(0);

pub fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-builder-box-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub struct Builder {
    pub child: std::process::Child,
    pub url: String,
    pub dir: PathBuf,
    pub node: Arc<Node>,
    pub reviewed: Arc<Mutex<Vec<u64>>>,
    pub said: Arc<Mutex<String>>,
}

impl Drop for Builder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Builder {
    pub fn store(&self) -> PathBuf {
        self.dir.join("store")
    }

    pub fn builds(&self) -> PathBuf {
        self.dir.join("builds")
    }

    /// `queue.json` as the builder last wrote it.
    pub fn queue_file(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.store().join("queue.json")).unwrap()).unwrap()
    }

    pub async fn upload(&self, author: Address, archive: &[u8]) -> (u16, String) {
        let reply = reqwest::Client::new()
            .post(format!("{}/base/uploads", self.url))
            .json(&shared::Upload {
                author: format!("{author:?}"),
                archive_base64: shared::base64::encode(archive),
                sealed: false,
                package: None,
            })
            .send()
            .await
            .unwrap();
        (reply.status().as_u16(), reply.text().await.unwrap())
    }

    /// Every job the builder holds, paid for or not, as `queue.json` last
    /// had it (the public list leaves out uploads not yet paid for).
    pub fn jobs(&self) -> Vec<Job> {
        let Ok(bytes) = std::fs::read(self.store().join("queue.json")) else {
            return Vec::new();
        };
        let written: Value = serde_json::from_slice(&bytes).unwrap();
        written["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| serde_json::from_value(entry["job"].clone()).unwrap())
            .collect()
    }

    /// The public queue.
    pub async fn listed(&self) -> Vec<Job> {
        reqwest::get(format!("{}/jobs", self.url))
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    /// One job, as asked for by its id.
    pub async fn job(&self, id: &str) -> Option<Job> {
        let reply = reqwest::get(format!("{}/jobs/{id}", self.url))
            .await
            .unwrap();
        if reply.status() == 404 {
            return None;
        }
        Some(reply.json().await.unwrap())
    }

    /// Wait up to `within` for `ready` to hold of every job the builder
    /// holds.
    pub async fn wait_for(&self, within: Duration, ready: impl Fn(&[Job]) -> bool) -> Vec<Job> {
        let started = Instant::now();
        loop {
            let jobs = self.jobs();
            if ready(&jobs) || started.elapsed() > within {
                return jobs;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// Start the builder binary, its store already holding each of `files`
/// (`queue.json`, say) as given, without waiting for it to open.
pub async fn spawn(node: Node, files: &[(&str, String)]) -> Builder {
    let dir = scratch();
    let store = dir.join("store");
    std::fs::create_dir_all(&store).unwrap();
    for (name, body) in files {
        let path = store.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let node = Arc::new(node);
    let node_at = serve(Router::new().route("/", post(rpc)).with_state(node.clone())).await;
    let reviewed: Arc<Mutex<Vec<u64>>> = Default::default();
    let reviewer_at = serve(
        Router::new()
            .route("/{chain}/reviews", post(review))
            .with_state(reviewed.clone()),
    )
    .await;

    std::fs::write(
        dir.join("chains.toml"),
        format!(
            "[chains.\"evm:31337\"]\nlabel = \"stand-in\"\nrpc = \"http://{node_at}\"\n\
             dollar = \"USDC\"\n{}[networks.local]\nbase = \"evm:31337\"\n",
            node.confirmations
                .map(|depth| format!("confirmations = {depth}\n"))
                .unwrap_or_default()
        ),
    )
    .unwrap();
    let deployments_dir = dir.join("deployments");
    std::fs::create_dir_all(&deployments_dir).unwrap();
    let zero = json!(format!("{:?}", Address::ZERO));
    let mut deployments = json!({
        "chain_key": "evm:31337", "chain_id": 31337, "rpc_url": "", "deployed_block": 0,
    });
    let mut roles = serde_json::Map::new();
    for role in [
        "admin", "platform", "attestor", "builder", "reviewer", "executor", "keeper", "operator",
        "author", "viewer",
    ] {
        roles.insert(role.to_string(), zero.clone());
    }
    roles.insert("builder".into(), json!(format!("{BUILDER_KEY:?}")));
    deployments["roles"] = roles.into();
    for name in [
        "usd",
        "venue",
        "attestation_verifier",
        "attestation",
        "prices",
        "strategies",
        "reviews",
        "profiles",
        "factory",
        "station_factory",
        "driver",
        "bridge",
    ] {
        deployments[name] = zero.clone();
    }
    deployments["publishing"] = json!(format!("{PUBLISHING:?}"));
    std::fs::write(
        deployments_dir.join("evm-31337.json"),
        deployments.to_string(),
    )
    .unwrap();

    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_builder"))
        .args(["--network", "local", "--vault-chain", "base", "--chains"])
        .arg(dir.join("chains.toml"))
        .arg("--deployments-dir")
        .arg(&deployments_dir)
        .arg("--store")
        .arg(&store)
        .arg("--builds")
        .arg(dir.join("builds"))
        .arg("--sdk")
        .arg(dir.join("sdk"))
        .arg("--vendor")
        .arg(vendor(&dir))
        .arg("--jobs")
        .arg(dir.join("jobs"))
        .arg("--reviewer-url")
        .arg(format!("http://{reviewer_at}"))
        .args(["--port", &port.to_string()])
        .env_remove("SHONIN_DEPLOYMENTS_JSON")
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "info")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let said = Arc::new(Mutex::new(String::new()));
    for stream in [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let said = said.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stream)
                .lines()
                .map_while(Result::ok)
            {
                let mut said = said.lock().unwrap();
                said.push_str(&line);
                said.push('\n');
            }
        });
    }
    Builder {
        child,
        url: format!("http://127.0.0.1:{port}"),
        dir,
        node,
        reviewed,
        said,
    }
}

/// [`spawn`], then wait for it to open and to have taken its chain on.
pub async fn start(node: Node, files: &[(&str, String)]) -> Builder {
    let builder = spawn(node, files).await;
    let http = reqwest::Client::new();
    let started = Instant::now();
    while http
        .get(format!("{}/health", builder.url))
        .send()
        .await
        .is_err()
        || !builder
            .said
            .lock()
            .unwrap()
            .contains("taking uploads and building jobs on this chain")
    {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the builder never opened:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    builder
}

/// A queue as `queue.json` holds it.
pub fn queue(jobs: Vec<Value>) -> (&'static str, String) {
    let next_id = 8_800 + jobs.len() as u64;
    (
        "queue.json",
        json!({"next_id": next_id, "jobs": jobs}).to_string(),
    )
}

/// A vendored copy of the platform's reviewed crates in `dir`, as the
/// builder checks it when it starts: a directory for each, holding the
/// checksum Cargo reads. Where it is.
pub fn vendor(dir: &std::path::Path) -> PathBuf {
    let vendor = dir.join("vendor");
    for listed in shared::dependencies::Allowlist::platform().crates() {
        let crate_dir = vendor.join(listed.directory());
        std::fs::create_dir_all(&crate_dir).unwrap();
        std::fs::write(
            crate_dir.join(".cargo-checksum.json"),
            json!({"files": {}, "package": listed.checksum}).to_string(),
        )
        .unwrap();
    }
    vendor
}

/// A `Cargo.lock` locking `packages`, the project's own, and `crates`, each
/// a crates.io crate by name, version and checksum.
pub fn lock(packages: &[&str], crates: &[(&str, &str, &str)]) -> String {
    let mut lock = "# This file is automatically @generated by Cargo.\nversion = 4\n".to_string();
    for package in packages {
        lock.push_str(&format!(
            "\n[[package]]\nname = \"{package}\"\nversion = \"0.1.0\"\n"
        ));
    }
    for (name, version, checksum) in crates {
        lock.push_str(&format!(
            "\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = \"{}\"\n\
             checksum = \"{checksum}\"\n",
            shared::dependencies::CRATES_IO
        ));
    }
    lock
}

/// A project the build service takes: one agent, on base, trading through
/// the local driver. `tag` makes each one's bytes its own.
pub fn project(tag: &str) -> Vec<u8> {
    let root = scratch().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"agent\"\n").unwrap();
    std::fs::write(root.join("Cargo.lock"), lock(&["agent"], &[])).unwrap();
    std::fs::write(
        root.join("shonin.toml"),
        "[vault]\nchain = \"base\"\n[description]\ntitle = \"An agent\"\ntext = \"x\"\n\
         [drivers.local]\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), format!("// {tag}\n")).unwrap();
    let packed = shared::archive::pack(&root).unwrap();
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
    packed
}

/// A job as `queue.json` holds it, submitted `age` ago: as a queue written
/// before each archive had a file of its own held it, archive and all, so
/// every test starts from a queue the builder moves the archives out of.
pub fn a_job(
    id: &str,
    author: Address,
    strategy_id: Option<u64>,
    state: JobState,
    age: time::Duration,
) -> Value {
    let now = time::OffsetDateTime::now_utc() - age;
    let job = Job {
        id: id.to_string(),
        chain: shared::Chain::Base,
        strategy_id,
        title: "t".to_string(),
        author: format!("{author:#x}"),
        state,
        submitted: time::PrimitiveDateTime::new(now.date(), now.time()),
        duration_seconds: 0,
        source_hash: format!("sha256:{}", "ab".repeat(32)),
        sealed: false,
        bundle_hash: None,
        review: None,
        strategy_slug: None,
    };
    json!({
        "job": serde_json::to_value(job).unwrap(),
        "upload": format!("0x{}", "44".repeat(32)),
        "author": format!("{author:?}"),
        "archive": shared::base64::encode(&project(id)),
        "manifest_path": "shonin.toml",
        "package": "agent",
    })
}

/// An upload as `queue.json` holds it, waiting for its payment against
/// `upload` since `age` ago, of `archive` as it would be sent again.
pub fn waiting(
    id: &str,
    author: Address,
    upload: B256,
    archive: &[u8],
    age: time::Duration,
) -> Value {
    let mut job = a_job(id, author, None, JobState::AwaitingPayment, age);
    job["job"]["source_hash"] = json!(shared::manifest::hash_bytes(archive));
    job["upload"] = json!(format!("{upload:#x}"));
    job["archive"] = json!(shared::base64::encode(archive));
    job
}

/// Await every one of `futures` at once, so they are in flight together.
pub async fn all_at_once<F: std::future::Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut pending: Vec<_> = futures.into_iter().map(|f| Some(Box::pin(f))).collect();
    let mut results: Vec<Option<F::Output>> = (0..pending.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut all = true;
        for (slot, result) in pending.iter_mut().zip(results.iter_mut()) {
            if let Some(future) = slot {
                match future.as_mut().poll(cx) {
                    std::task::Poll::Ready(value) => {
                        *result = Some(value);
                        *slot = None;
                    }
                    std::task::Poll::Pending => all = false,
                }
            }
        }
        if all {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    results.into_iter().map(Option::unwrap).collect()
}
