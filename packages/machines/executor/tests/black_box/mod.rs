//! A stand-in node and store for black-box tests of the executor binary: an
//! open vault whose key is blessed, with the station's approval scripted by
//! `Script`. The vault's allowances can be raised while the executor runs,
//! and the node can lie about the usage digest.

#![allow(dead_code)]

#[path = "../status_port/mod.rs"]
mod status_port;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol_types::SolCall;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use chain::bindings::attestation::{IAttestationRegistry, ImageKind, KeyInfo};
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::strategies::{IStrategyRegistry, Strategy};
use chain::bindings::vault::{IVault, Tank, VaultState};
use chain::bindings::venue::ILocalVenue;
use serde_json::{Value, json};

pub const NOW: u64 = 1_700_000_000;
pub const TTL: u64 = 72 * 3_600;

pub fn vault() -> Address {
    Address::repeat_byte(0x11)
}

pub fn bundle() -> B256 {
    B256::repeat_byte(0x42)
}

/// Another executor's key, blessed as this vault's executor.
pub fn other_key() -> Address {
    Address::repeat_byte(0xee)
}

/// The key the executor derives for a vault (`key_for` in its `main.rs`).
pub fn executor_key(vault: Address) -> Address {
    let mut seed = chain::attest::image_digest("executor").to_vec();
    seed.extend_from_slice(vault.as_slice());
    PrivateKeySigner::from_bytes(&keccak256(seed))
        .unwrap()
        .address()
}

/// How the station's approval and the head move.
#[derive(Clone, Copy, PartialEq)]
pub enum Script {
    /// This executor's key is approved, the head stays at 0x10.
    Approved,
    /// Nobody is approved until the first `eth_blockNumber` (the stake's);
    /// then another executor's blessed key is, for `replaced_for`; then this
    /// executor's key again, with the head moved on to `later_head`.
    TakenOverThenBack {
        replaced_for: Duration,
        later_head: u64,
    },
}

pub struct Node {
    pub manifest_hash: B256,
    pub script: Script,
    /// When the first `eth_blockNumber` was asked.
    pub first_head_read: Mutex<Option<Instant>>,
    /// The `fromBlock` of every log request, in order.
    pub logs_from: Mutex<Vec<u64>>,
    /// Fail `state()` for this long after each `haltedAt()`, if set: a node
    /// that fails a read now and then.
    pub hiccup_after_halted_at: Option<Duration>,
    pub last_halted_at: Mutex<Option<Instant>>,
    /// How many `state()` reads were failed.
    pub hiccups: Mutex<u64>,
    /// The vault's `haltedAt()`: 0 until a test halts it.
    pub halted_at: AtomicU64,
    /// Whether the author has raised the vault's memory allowance.
    pub raised: std::sync::atomic::AtomicBool,
    /// If set, `usageDigest` answers with the digest the vault computes for
    /// this total, whatever total it is asked about: a node that lies.
    pub lying_total: Mutex<Option<U256>>,
}

/// The digest the vault signs a usage total under (`Upkeep.usageDigest`).
pub fn usage_digest(total: U256, as_of: u64) -> B256 {
    use alloy::sol_types::SolValue;
    keccak256(
        (
            U256::from(31337u64),
            vault(),
            String::from("shonin-usage"),
            total,
            as_of,
        )
            .abi_encode_params(),
    )
}

impl Node {
    fn phase(&self) -> u8 {
        match self.script {
            Script::Approved => 3,
            Script::TakenOverThenBack { replaced_for, .. } => {
                match *self.first_head_read.lock().unwrap() {
                    None => 1,
                    Some(at) if at.elapsed() < replaced_for => 2,
                    Some(_) => 3,
                }
            }
        }
    }

    fn head(&self) -> u64 {
        match self.script {
            Script::TakenOverThenBack { later_head, .. } if self.phase() == 3 => later_head,
            _ => 0x10,
        }
    }
}

fn word(value: u64) -> Vec<u8> {
    U256::from(value).to_be_bytes::<32>().to_vec()
}

fn answer_call(node: &Node, input: &[u8]) -> Option<Vec<u8>> {
    let selector: [u8; 4] = input.get(..4)?.try_into().ok()?;
    let until = NOW + TTL * 3 / 4;
    Some(match selector {
        IVaultFactory::vaultsCall::SELECTOR => {
            IVaultFactory::vaultsCall::abi_encode_returns(&vec![vault()])
        }
        IVault::stateCall::SELECTOR => IVault::stateCall::abi_encode_returns(&VaultState::Open),
        IStationVault::haltedAtCall::SELECTOR => word(node.halted_at.load(Ordering::SeqCst)),
        IStationVault::haltGraceCall::SELECTOR => word(600),
        IVault::bundleHashCall::SELECTOR => IVault::bundleHashCall::abi_encode_returns(&bundle()),
        IVault::homeStationCall::SELECTOR => {
            IVault::homeStationCall::abi_encode_returns(&Address::repeat_byte(0x55))
        }
        IVault::strategyIdCall::SELECTOR => word(7),
        IVault::allowancesCall::SELECTOR => {
            let mut allowances = shared::Allowances::modest();
            if node.raised.load(Ordering::SeqCst) {
                allowances.memory_mib *= 2;
            }
            IVault::allowancesCall::abi_encode_returns(&chain::allowances(&allowances))
        }
        IVault::tankCall::SELECTOR => IVault::tankCall::abi_encode_returns(&Tank {
            balance: U256::from(1_000_000_000_000u64),
            keepAt: U256::ZERO,
            usageSettled: U256::ZERO,
            usageAsOf: 0,
            lastCollection: 0,
            escapeRequestedAt: 0,
            capCarried: U256::ZERO,
            fillPending: false,
        }),
        IVault::usageDigestCall::SELECTOR => {
            let call = IVault::usageDigestCall::abi_decode(input).ok()?;
            let total = node.lying_total.lock().unwrap().unwrap_or(call.total);
            IVault::usageDigestCall::abi_encode_returns(&usage_digest(total, call.asOf))
        }
        IVault::cashCall::SELECTOR
        | IVault::sharePriceCall::SELECTOR
        | IVault::totalValueCall::SELECTOR
        | IVault::requestCountCall::SELECTOR
        | IVault::claimCountCall::SELECTOR => word(0),
        IVault::idleFloorCall::SELECTOR => [0u8; 96].to_vec(),
        IStation::approvedKeyCall::SELECTOR => {
            let approved = match node.phase() {
                1 => Address::ZERO,
                2 => other_key(),
                _ => executor_key(vault()),
            };
            IStation::approvedKeyCall::abi_encode_returns(&approved)
        }
        IStation::pinnedDriverCall::SELECTOR => {
            IStation::pinnedDriverCall::abi_encode_returns(&Address::repeat_byte(0x77))
        }
        IAttestationRegistry::isDriverRevokedCall::SELECTOR => {
            IAttestationRegistry::isDriverRevokedCall::abi_encode_returns(&false)
        }
        IAttestationRegistry::isBlessedForCall::SELECTOR => {
            IAttestationRegistry::isBlessedForCall::abi_encode_returns(&true)
        }
        IAttestationRegistry::blessingTtlCall::SELECTOR => word(TTL),
        IAttestationRegistry::blessedUntilCall::SELECTOR => word(until),
        ILocalVenue::agentValidUntilCall::SELECTOR => word(until),
        IAttestationRegistry::keyInfoCall::SELECTOR => {
            IAttestationRegistry::keyInfoCall::abi_encode_returns(&KeyInfo {
                imageDigest: B256::ZERO,
                projectId: B256::ZERO,
                kind: ImageKind::Executor,
                strategyHash: bundle(),
                vaultChain: U256::from(31337u64),
                vault: vault(),
                trustRoot: B256::ZERO,
                verifier: Address::ZERO,
                blessedUntil: until,
            })
        }
        IStrategyRegistry::strategyOfCall::SELECTOR => {
            IStrategyRegistry::strategyOfCall::abi_encode_returns(&Strategy {
                author: Address::ZERO,
                sourceHash: B256::ZERO,
                bundleHash: bundle(),
                manifestHash: node.manifest_hash,
                registeredAt: 0,
                chains: Vec::new(),
                bridges: Vec::new(),
                restricted: Vec::new(),
            })
        }
        _ => return None,
    })
}

fn hex_u64(value: &Value) -> Option<u64> {
    u64::from_str_radix(value.as_str()?.trim_start_matches("0x"), 16).ok()
}

async fn rpc(State(node): State<Arc<Node>>, Json(request): Json<Value>) -> Response {
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or_default();
    let result = match method {
        "eth_chainId" => json!("0x7a69"),
        "eth_accounts" => json!([]),
        "eth_blockNumber" => {
            node.first_head_read
                .lock()
                .unwrap()
                .get_or_insert_with(Instant::now);
            json!(format!("0x{:x}", node.head()))
        }
        "eth_getCode" => json!("0x01"),
        "eth_getLogs" => {
            if let Some(from) = hex_u64(&request["params"][0]["fromBlock"]) {
                node.logs_from.lock().unwrap().push(from);
            }
            json!([])
        }
        "eth_getBalance" => json!("0xffffffffffffffffffffff"),
        "eth_gasPrice" | "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
        "eth_feeHistory" => json!({
            "oldestBlock": "0x10",
            "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
            "gasUsedRatio": [0.5],
            "reward": [["0x1"]],
        }),
        "eth_getBlockByNumber" => {
            let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
            block.header.inner.number = node.head();
            block.header.inner.timestamp = NOW;
            block.header.inner.base_fee_per_gas = Some(1_000_000_000);
            serde_json::to_value(block).unwrap()
        }
        "eth_call" => {
            let tx = &request["params"][0];
            let data = tx["input"]
                .as_str()
                .or_else(|| tx["data"].as_str())
                .unwrap_or("0x");
            let input = alloy::hex::decode(data).unwrap_or_default();
            if input.starts_with(&IStationVault::haltedAtCall::SELECTOR) {
                *node.last_halted_at.lock().unwrap() = Some(Instant::now());
            }
            if input.starts_with(&IVault::stateCall::SELECTOR)
                && let Some(window) = node.hiccup_after_halted_at
                && node
                    .last_halted_at
                    .lock()
                    .unwrap()
                    .is_some_and(|at| at.elapsed() < window)
            {
                *node.hiccups.lock().unwrap() += 1;
                return Json(json!({"jsonrpc": "2.0", "id": id,
                    "error": {"code": -32000, "message": "header not found"}}))
                .into_response();
            }
            match answer_call(&node, &input) {
                Some(out) => json!(alloy::hex::encode_prefixed(out)),
                None => {
                    return Json(json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": 3, "message": "execution reverted"}}))
                    .into_response();
                }
            }
        }
        _ => {
            return Json(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": "method not found"}}))
            .into_response();
        }
    };
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

/// A manifest naming the local driver, as the builder records it for the
/// vault's bundle and `component`, filed under its own hash.
fn write_manifest(store: &Path, component: &[u8]) -> B256 {
    let manifest = shared::Manifest::from_toml(
        "[vault]\nchain = \"base\"\n[description]\ntitle = \"t\"\ntext = \"x\"\n[drivers.local]\n",
    )
    .unwrap();
    let canonical = shared::Canonical {
        strategy_id: 7,
        vault_chain: shared::Chain::Base,
        author: "0xa11ce".into(),
        bundle_hash: format!("sha256:{}", alloy::hex::encode(bundle())),
        component_hash: shared::manifest::hash_bytes(component),
        source_hash: "sha256:00".into(),
        sealed: false,
        world: "shonin:agent@0.1.0".into(),
        toolchain: "rust".into(),
        builder_image: "sha256:builder".into(),
        manifest,
    };
    let hash = canonical.hash();
    let directory = store.join(hash.replace(':', "-"));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec(&canonical).unwrap(),
    )
    .unwrap();
    B256::from_slice(&alloy::hex::decode(hash.trim_start_matches("sha256:")).unwrap())
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-executor-black-box-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The executor process, killed when dropped, and everything it said.
pub struct Running {
    child: std::process::Child,
    pub said: Arc<Mutex<String>>,
    pub node: Arc<Node>,
    pub port: u16,
    pub dir: PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Running {
    /// How many lines it has said containing `text`.
    pub fn lines_with(&self, text: &str) -> usize {
        self.said
            .lock()
            .unwrap()
            .lines()
            .filter(|line| line.contains(text))
            .count()
    }
}

/// Start the executor binary against a stand-in node scripted by `script`,
/// with `component` as the vault's bundle.
pub async fn start(component: &str, script: Script) -> Running {
    start_with(component, script, None).await
}

/// [`start`], with the node failing `state()` for `hiccup` after every
/// `haltedAt()`.
pub async fn start_with(component: &str, script: Script, hiccup: Option<Duration>) -> Running {
    start_recording(component, component, script, hiccup).await
}

/// [`start_with`], the manifest recording `recorded` as the component while
/// the store holds `component`.
pub async fn start_recording(
    recorded: &str,
    component: &str,
    script: Script,
    hiccup: Option<Duration>,
) -> Running {
    let dir = scratch();
    let store = dir.join("store");
    let manifest_hash = write_manifest(&store, recorded.as_bytes());
    let path = store
        .join(format!("sha256-{}", alloy::hex::encode(bundle())))
        .join("component.wasm");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, component).unwrap();

    let node = Arc::new(Node {
        manifest_hash,
        script,
        first_head_read: Mutex::new(None),
        logs_from: Mutex::new(Vec::new()),
        hiccup_after_halted_at: hiccup,
        last_halted_at: Mutex::new(None),
        hiccups: Mutex::new(0),
        halted_at: AtomicU64::new(0),
        raised: Default::default(),
        lying_total: Mutex::new(None),
    });
    let app = Router::new().route("/", post(rpc)).with_state(node.clone());
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    std::fs::write(
        dir.join("chains.toml"),
        format!(
            "[chains.\"evm:31337\"]\nlabel = \"stand-in\"\nrpc = \"http://{address}\"\n\
             dollar = \"USDC\"\nnative_usd = 2500.0\n[networks.local]\nbase = \"evm:31337\"\n"
        ),
    )
    .unwrap();
    let deployments_dir = dir.join("deployments");
    std::fs::create_dir_all(&deployments_dir).unwrap();
    let zero = json!(format!("{:?}", Address::ZERO));
    let mut deployments = json!({
        "chain_key": "evm:31337", "chain_id": 31337, "rpc_url": "", "deployed_block": 0,
    });
    deployments["roles"] = [
        "admin", "platform", "attestor", "builder", "reviewer", "executor", "keeper", "operator",
        "author", "viewer",
    ]
    .iter()
    .map(|role| (role.to_string(), zero.clone()))
    .collect::<serde_json::Map<_, _>>()
    .into();
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
        deployments[name] = zero.clone();
    }
    std::fs::write(
        deployments_dir.join("evm-31337.json"),
        deployments.to_string(),
    )
    .unwrap();

    let started = status_port::start(
        std::process::Command::new(env!("CARGO_BIN_EXE_executor"))
            .args(["--network", "local", "--vault-chain", "base", "--chains"])
            .arg(dir.join("chains.toml"))
            .arg("--deployments-dir")
            .arg(&deployments_dir)
            .arg("--store")
            .arg(&store)
            .args(["--api-url", "http://127.0.0.1:9"])
            .env_remove("SHONIN_DEPLOYMENTS_JSON"),
    )
    .await;
    let status_port::Started { child, said, port } = started;
    Running {
        child,
        said,
        node,
        port,
        dir,
    }
}
