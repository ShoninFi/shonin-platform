//! A strategy's own words are cut to 1 KiB before they reach the log,
//! however it fails: when its `run` faults, and when it fails while it is
//! being instantiated, where a trap carries a wasm backtrace naming the
//! guest's functions from its own name section, at whatever length the
//! author gave them. The agent is started again after a fault, so a strategy
//! whose constructor traps would otherwise write its names into the
//! executor's log at that rate, without bound.
//!
//! This runs the executor binary against a stand-in node answering as an
//! open vault does, and a store holding a component that traps in its start
//! function, a function named with 64 KiB, and reads the log.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

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

const NOW: u64 = 1_700_000_000;
const TTL: u64 = 72 * 3_600;

fn vault() -> Address {
    Address::repeat_byte(0x11)
}

fn bundle() -> B256 {
    B256::repeat_byte(0x42)
}

/// The key the executor derives for a vault (`key_for` in its `main.rs`).
fn executor_key(vault: Address) -> Address {
    let mut seed = chain::attest::image_digest("executor").to_vec();
    seed.extend_from_slice(vault.as_slice());
    PrivateKeySigner::from_bytes(&keccak256(seed))
        .unwrap()
        .address()
}

struct Node {
    manifest_hash: B256,
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
        IStationVault::haltedAtCall::SELECTOR => word(0),
        IStationVault::haltGraceCall::SELECTOR => word(600),
        IVault::bundleHashCall::SELECTOR => IVault::bundleHashCall::abi_encode_returns(&bundle()),
        IVault::homeStationCall::SELECTOR => {
            IVault::homeStationCall::abi_encode_returns(&Address::repeat_byte(0x55))
        }
        IVault::strategyIdCall::SELECTOR => word(7),
        IVault::allowancesCall::SELECTOR => IVault::allowancesCall::abi_encode_returns(
            &chain::allowances(&shared::Allowances::modest()),
        ),
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
            IVault::usageDigestCall::abi_encode_returns(&keccak256(input))
        }
        IVault::cashCall::SELECTOR
        | IVault::sharePriceCall::SELECTOR
        | IVault::totalValueCall::SELECTOR
        | IVault::requestCountCall::SELECTOR
        | IVault::claimCountCall::SELECTOR => word(0),
        IVault::idleFloorCall::SELECTOR => [0u8; 96].to_vec(),
        IStation::approvedKeyCall::SELECTOR => {
            IStation::approvedKeyCall::abi_encode_returns(&executor_key(vault()))
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

async fn rpc(State(node): State<Arc<Node>>, Json(request): Json<Value>) -> Response {
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or_default();
    let result = match method {
        "eth_chainId" => json!("0x7a69"),
        "eth_accounts" => json!([]),
        "eth_blockNumber" => json!("0x10"),
        "eth_getCode" => json!("0x01"),
        "eth_getLogs" => json!([]),
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
            block.header.inner.number = 16;
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
        "shonin-executor-component-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Killed(std::process::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The length of the guest's function name.
const NAME: usize = 64 * 1024;

fn traps_while_instantiating() -> String {
    let name = "a".repeat(NAME);
    format!(
        r#"(component
    (core module $m
        (memory (export "memory") 1)
        (func ${name} unreachable)
        (start ${name})
        (func (export "run") (result i32) (i32.const 0)))
    (core instance $i (instantiate $m))
    (func $run async (result (result (error string)))
        (canon lift (core func $i "run") (memory (core memory $i "memory"))))
    (export "run" (func $run)))"#
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_strategy_s_words_reach_the_log_cut_to_a_kibibyte_however_it_fails() {
    let component = traps_while_instantiating();
    let dir = scratch();
    let store = dir.join("store");
    let manifest_hash = write_manifest(&store, component.as_bytes());
    let path = store
        .join(format!("sha256-{}", alloy::hex::encode(bundle())))
        .join("component.wasm");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &component).unwrap();

    let app = Router::new()
        .route("/", post(rpc))
        .with_state(Arc::new(Node { manifest_hash }));
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let node = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    std::fs::write(
        dir.join("chains.toml"),
        format!(
            "[chains.\"evm:31337\"]\nlabel = \"stand-in\"\nrpc = \"http://{node}\"\n\
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

    let log = dir.join("executor.log");
    let executor = Killed(
        std::process::Command::new(env!("CARGO_BIN_EXE_executor"))
            .args(["--network", "local", "--vault-chain", "base", "--chains"])
            .arg(dir.join("chains.toml"))
            .arg("--deployments-dir")
            .arg(&deployments_dir)
            .arg("--store")
            .arg(&store)
            .args(["--api-url", "http://127.0.0.1:9"])
            .env_remove("SHONIN_DEPLOYMENTS_JSON")
            .env("RUST_LOG", "info")
            .stdout(std::fs::File::create(&log).unwrap())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::sleep(Duration::from_secs(20)).await;
    drop(executor);
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let longest = said
        .split(|c: char| c != 'a')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let lines = said
        .lines()
        .filter(|line| line.contains(&"a".repeat(2_048)))
        .count();
    assert!(
        said.contains("could not be run") || said.contains("stopped"),
        "the agent was never started: {}",
        &said[..said.len().min(2_000)]
    );
    assert!(
        longest <= 1_024,
        "{lines} log lines carried {longest} bytes of the strategy's own function name; the \
         log is {} bytes after 20 s",
        said.len()
    );
}
