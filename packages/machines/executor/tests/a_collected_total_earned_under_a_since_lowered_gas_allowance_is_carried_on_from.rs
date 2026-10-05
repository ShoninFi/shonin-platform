//! A collected total the vault really took is carried on from, however it
//! was earned. A vault whose author allowed, say, $600 a day of gas for its
//! first ten days and spent it, then lowered the gas allowance to nothing,
//! has collected about $5,000: the contract held each collection to the
//! allowances in force over its stretch, gas included. An executor that
//! takes the vault on after the change starts its meter at zero and reads
//! that $5,000 as collected. Were it to sign below it, every `keep` would
//! revert `Stale` and the unpaid part it compares with the tank would be
//! nothing, so the agent would run unbilled and never be stopped for a dry
//! tank. So once two reads of the figure agree, it signs on top of it, not
//! held to what the allowances in force now (or this executor's ceilings,
//! lower than an earlier executor's) could have run up.
//!
//! This runs the executor binary against a stand-in node answering as an
//! open vault does, its key blessed and approved, the head moving on.

mod status_port;

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
    head: std::sync::atomic::AtomicU64,
}

/// What the vault has collected: ten days of a predecessor's totals, most of
/// it gas spent under a $600-a-day gas allowance the author has since cut
/// to nothing.
const COLLECTED: u64 = 5_000_000_000;
const TEN_DAYS: u64 = 10 * 86_400;

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
        IVault::allowancesCall::SELECTOR => {
            IVault::allowancesCall::abi_encode_returns(&chain::allowances(&shared::Allowances {
                gas_per_day: 0.0,
                ..shared::Allowances::modest()
            }))
        }
        IVault::tankCall::SELECTOR => IVault::tankCall::abi_encode_returns(&Tank {
            balance: U256::from(1_000_000_000_000u64),
            keepAt: U256::ZERO,
            usageSettled: U256::from(COLLECTED),
            usageAsOf: NOW - 3_600,
            lastCollection: 0,
            escapeRequestedAt: 0,
            capCarried: U256::ZERO,
            fillPending: false,
        }),
        IVault::createdAtCall::SELECTOR => word(NOW - TEN_DAYS),
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
        // The chain moves on: a block for every look at the head.
        "eth_blockNumber" => json!(format!(
            "0x{:x}",
            node.head.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        )),
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

/// A component that runs until it is stopped. `$tag` tells one copy from
/// another.
fn a_component(tag: u32) -> String {
    format!(
        r#"(component
    (core module $m
        (memory (export "memory") 1)
        (global $tag i32 (i32.const {tag}))
        (func (export "run") (result i32)
            (loop $on (br $on))
            (i32.const 0)))
    (core instance $i (instantiate $m))
    (func $run async (result (result (error string)))
        (canon lift (core func $i "run") (memory (core memory $i "memory"))))
    (export "run" (func $run)))"#
    )
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

/// Start an executor over a store whose manifest records `recorded` and which
/// holds `stored`.
async fn start(recorded: &str, stored: &str) -> (status_port::Started, PathBuf) {
    let dir = scratch();
    let store = dir.join("store");
    let manifest_hash = write_manifest(&store, recorded.as_bytes());
    let component = store
        .join(format!("sha256-{}", alloy::hex::encode(bundle())))
        .join("component.wasm");
    std::fs::create_dir_all(component.parent().unwrap()).unwrap();
    std::fs::write(&component, stored).unwrap();

    let app = Router::new()
        .route("/", post(rpc))
        .with_state(Arc::new(Node {
            manifest_hash,
            head: std::sync::atomic::AtomicU64::new(16),
        }));
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

    let executor = status_port::start(
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
    (executor, dir)
}

/// The latest total the executor serves after `watch`.
async fn latest(port: u16, watch: Duration) -> Option<u64> {
    let url = format!("http://127.0.0.1:{port}/usage/31337/{:?}", vault());
    let http = reqwest::Client::new();
    let mut signed = None;
    let started = std::time::Instant::now();
    while started.elapsed() < watch {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(reply) = http.get(&url).send().await
            && reply.status().is_success()
            && let Ok(body) = reply.json::<Value>().await
        {
            signed = body["total"].as_str().and_then(|t| t.parse::<u64>().ok());
        }
    }
    signed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_collected_total_earned_under_a_since_lowered_gas_allowance_is_carried_on_from() {
    // Nothing is run (the stored component isn't the bundle's), so nothing
    // new accrues: the right total to sign is what the vault has collected.
    let (mut executor, dir) = start(&a_component(1), &a_component(2)).await;
    // Many ticks, the head moving at every one: any agreement is reached.
    let total = latest(executor.port, Duration::from_secs(25)).await;
    let said = executor.said.lock().unwrap().clone();
    let _ = executor.child.kill();
    let _ = executor.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    let total = total.expect("the executor served no total");
    assert!(
        total >= COLLECTED,
        "the vault has collected {COLLECTED}, and the executor signs {total}: every `keep` \
         with it reverts Stale, so nothing is billed again and the tank never reads dry\n{}",
        said.lines()
            .filter(|line| line.contains("collected total"))
            .take(2)
            .collect::<Vec<_>>()
            .join("\n")
    );
}
