//! The fuel budget holds an agent to the processor its tank pays for, the
//! host's work for it included: an agent that asks the host for as much
//! work a call as it may (here `wasi:random/random.get-random-bytes` of the
//! most a call may ask for, a few fuel of its own for the host's making and
//! copying them, the same shape as a `wasi:keyvalue` get or set of a large
//! value, or a large inference input) is charged for the time the host
//! spends answering it, and parked to repay it, so it takes no more of the
//! processor than an agent that spins.
//!
//! This runs the executor binary against a stand-in node answering as an
//! open vault does at the modest allowances, its key blessed and approved,
//! and a store holding a component whose `run` loops over that call. The
//! processor the executor burns over ten seconds is weighed against what
//! the allowance pays for.

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

/// A component whose `run` asks the host for a MiB of random bytes (the
/// most a call may ask for), over and over: one call, a few fuel, and the
/// host's processor for as long as it takes to make and copy them. The memory and realloc the lowering
/// needs live in a module of their own, instantiated first.
const ASKS_THE_HOST: &str = r#"(component
    (import "wasi:random/random@0.2.12" (instance $random
        (export "get-random-bytes" (func (param "len" u64) (result (list u8))))))
    (core module $mem
        (memory (export "memory") 600)
        (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) (i32.const 65536)))
    (core instance $mem (instantiate $mem))
    (alias core export $mem "memory" (core memory $memory))
    (alias core export $mem "cabi_realloc" (core func $realloc))
    (core func $bytes (canon lower (func $random "get-random-bytes")
        (memory $memory) (realloc $realloc)))
    (core instance $env (export "memory" (memory $memory)))
    (core instance $wasi (export "get-random-bytes" (func $bytes)))
    (core module $m
        (import "env" "memory" (memory 600))
        (import "wasi" "get-random-bytes" (func $bytes (param i64 i32)))
        (func (export "run") (result i32)
            (loop $on
                (call $bytes (i64.const 1048576) (i32.const 0))
                (br $on))
            (i32.const 0)))
    (core instance $i (instantiate $m (with "env" (instance $env)) (with "wasi" (instance $wasi))))
    (func $run async (result (result (error string)))
        (canon lift (core func $i "run") (memory $memory)))
    (export "run" (func $run)))"#;

/// The processor time a process has used so far, in seconds.
fn cpu_seconds(pid: u32) -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // [[dd-]hh:]mm:ss[.ff]
    let text = text.rsplit('-').next().unwrap_or("0").to_string();
    text.split(':').fold(0.0, |total, part| {
        total * 60.0 + part.parse::<f64>().unwrap_or(0.0)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_whose_calls_keep_the_host_busy_uses_the_processor_its_fuel_allowance_pays_for() {
    let component = ASKS_THE_HOST;
    let dir = scratch();
    let store = dir.join("store");
    let manifest_hash = write_manifest(&store, component.as_bytes());
    let path = store
        .join(format!("sha256-{}", alloy::hex::encode(bundle())))
        .join("component.wasm");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, component).unwrap();

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
    let port = started.port;
    let said = started.said.clone();
    let executor = Killed(started.child);
    let pid = executor.0.id();

    // Running once its meter signs a total.
    let url = format!("http://127.0.0.1:{port}/usage/31337/{:?}", vault());
    let http = reqwest::Client::new();
    let started = std::time::Instant::now();
    let mut running = false;
    while !running && started.elapsed() < Duration::from_secs(60) {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(reply) = http.get(&url).send().await
            && reply.status().is_success()
            && let Ok(body) = reply.json::<Value>().await
        {
            running = body["total"]
                .as_str()
                .and_then(|t| t.parse::<u64>().ok())
                .is_some_and(|total| total > 0);
        }
    }
    assert!(running, "the executor never took the vault on");

    // Ten seconds of the agent asking the host, once it has been compiled
    // and started.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let before = cpu_seconds(pid);
    let over = Duration::from_secs(10);
    tokio::time::sleep(over).await;
    let burned = cpu_seconds(pid) - before;
    drop(executor);
    let _ = std::fs::remove_dir_all(&dir);

    // Its calls were answered: it ran throughout, never stopped by a call
    // it may not make.
    let said = said.lock().unwrap().clone();
    assert!(
        !said.contains("the strategy stopped"),
        "the agent stopped:\n{said}"
    );

    // The modest shape's ten million fuel a second is a hundredth of a
    // billed core: a tenth of a second over ten; the rest allows for the
    // executor's own work. The agent's own fuel is a few units a call: it
    // is the host's time answering it that its budget is charged.
    let billed = shared::Allowances::modest().fuel_per_sec as f64
        / shared::manifest::FUEL_PER_CORE_SECOND as f64
        * over.as_secs_f64();
    assert!(
        burned < billed + 0.5,
        "an agent declared at the modest shape's fuel, asking the host for a MiB of random \
         bytes in a loop, took {burned:.2} s of processor in {} s; its fuel pays for {billed:.2} s",
        over.as_secs()
    );
}
