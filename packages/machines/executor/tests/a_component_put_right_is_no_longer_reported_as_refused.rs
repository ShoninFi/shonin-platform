//! A component in the store that is not the vault's bundle is refused, and
//! the vault's health report says so at Error. Once the store is put right
//! (a store being rewritten, a file read mid-write at a restart) and the
//! vault's own component runs, the report no longer says it: refusals are
//! reported for as long as the latest start was refused, not for as long as
//! the executor serves the vault.
//!
//! The executor binary runs against a stand-in node answering as an open
//! vault does, its key blessed and approved; the store first holds a
//! component that is not the bundle's, then the right one. Reports are
//! posted to a stand-in API.

mod status_port;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, B256, U256, keccak256};
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

/// The executor's key for the vault: made inside the executor, so learnt
/// here from its first question to the registry about its own key.
static EXECUTOR_KEY: std::sync::Mutex<Option<Address>> = std::sync::Mutex::new(None);

/// The key the station approves: none until the executor's own key has
/// asked whether it is blessed (as it does before it asks for the
/// approval), then that key, as the approval it would send leaves it.
fn executor_key(_vault: Address) -> Address {
    EXECUTOR_KEY.lock().unwrap().unwrap_or(Address::ZERO)
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
            if let Ok(call) = IAttestationRegistry::isBlessedForCall::abi_decode(input)
                && call.signer != Address::ZERO
            {
                EXECUTOR_KEY.lock().unwrap().get_or_insert(call.signer);
            }
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

struct Killed(std::process::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The health reports the executor posts, newest last, with when each came.
type Posted = Arc<std::sync::Mutex<Vec<(std::time::Instant, shared::HealthPost)>>>;

async fn a_health_api() -> (String, Posted) {
    let posted: Posted = Default::default();
    let keep = posted.clone();
    let app = Router::new().route(
        "/api/{chain}/health",
        post(move |Json(post): Json<shared::HealthPost>| {
            let keep = keep.clone();
            async move {
                keep.lock().unwrap().push((std::time::Instant::now(), post));
                axum::http::StatusCode::ACCEPTED
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, posted)
}

fn says_not_the_bundle(post: &shared::HealthPost) -> bool {
    post.report
        .entries
        .iter()
        .any(|entry| entry.message.contains("not the vault's bundle"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_component_put_right_is_no_longer_reported_as_refused() {
    let dir = scratch();
    let store = dir.join("store");
    let right = a_component(1);
    let manifest_hash = write_manifest(&store, right.as_bytes());
    let component = store
        .join(format!("sha256-{}", alloy::hex::encode(bundle())))
        .join("component.wasm");
    std::fs::create_dir_all(component.parent().unwrap()).unwrap();
    // Briefly not the bundle's component.
    std::fs::write(&component, a_component(2)).unwrap();

    let app = Router::new()
        .route("/", post(rpc))
        .with_state(Arc::new(Node { manifest_hash }));
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let node = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (api, posted) = a_health_api().await;

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

    let started = status_port::start(
        std::process::Command::new(env!("CARGO_BIN_EXE_executor"))
            .args(["--network", "local", "--vault-chain", "base", "--chains"])
            .arg(dir.join("chains.toml"))
            .arg("--deployments-dir")
            .arg(&deployments_dir)
            .arg("--store")
            .arg(&store)
            .args(["--api-url", &api])
            .env_remove("SHONIN_DEPLOYMENTS_JSON"),
    )
    .await;
    let port = started.port;
    let executor = Killed(started.child);

    // The first serves find the wrong component and refuse it, and a report
    // says so; then the store is put right.
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(40)
        && !posted
            .lock()
            .unwrap()
            .iter()
            .any(|(_, post)| says_not_the_bundle(post))
    {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        posted
            .lock()
            .unwrap()
            .iter()
            .any(|(_, post)| says_not_the_bundle(post)),
        "the wrong component was never reported as refused"
    );
    std::fs::write(&component, &right).unwrap();

    // Wait until the right component is billed as running.
    let url = format!("http://127.0.0.1:{port}/usage/31337/{:?}", vault());
    let http = reqwest::Client::new();
    let mut running_since = None;
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(90) {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(reply) = http.get(&url).send().await
            && reply.status().is_success()
            && let Ok(body) = reply.json::<Value>().await
            && body["total"]
                .as_str()
                .and_then(|t| t.parse::<u64>().ok())
                .is_some_and(|total| total > 0)
        {
            running_since = Some(std::time::Instant::now());
            break;
        }
    }
    let running_since = running_since.expect("the right component was run and billed");

    // Two reports after that: well after the right component has run.
    let mut after = Vec::new();
    while started.elapsed() < Duration::from_secs(150) && after.len() < 2 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        after = posted
            .lock()
            .unwrap()
            .iter()
            .filter(|(at, _)| *at > running_since)
            .map(|(_, post)| post.clone())
            .collect();
    }
    let refused_before = posted
        .lock()
        .unwrap()
        .iter()
        .any(|(_, post)| says_not_the_bundle(post));
    drop(executor);
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!after.is_empty(), "no report came after the agent ran");
    let latest = after.last().unwrap();
    assert!(
        !says_not_the_bundle(latest),
        "the vault's own component has been running for {:?}, and its latest report still \
         says, at Error: {:?} (refused before: {refused_before})",
        running_since.elapsed(),
        latest
            .report
            .entries
            .iter()
            .filter(|entry| entry.message.contains("bundle"))
            .map(|entry| (&entry.level, &entry.message))
            .collect::<Vec<_>>()
    );
}
