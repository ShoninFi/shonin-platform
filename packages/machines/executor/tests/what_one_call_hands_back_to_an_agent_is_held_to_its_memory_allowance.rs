//! The host lowers every answer it gives an agent into the agent's own
//! memory, so it never builds an answer bigger than the agent's memory
//! allowance: one call that would hand back more is refused with the
//! interface's error before anything is copied. A `wasi:keyvalue/batch`
//! `get-many` naming one stored key many times is such a call: each key
//! named is a value handed back, so the answer is weighed as every copy it
//! would hold.
//!
//! This runs the executor binary against a stand-in node answering as an
//! open vault does at the modest allowances (64 MiB of memory), its key
//! blessed and approved, and a store holding a component whose `run` stores
//! a MiB under one key and then asks `get-many` for that key 2,048 times,
//! over and over. The executor's resident memory is sampled while the agent
//! runs, and its peak weighed against the agent's allowance and what the
//! executor itself needs.

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

/// How many times one key is named in each `get-many`.
const REPEATS: u32 = 2_048;

/// The value stored under it: a MiB.
const VALUE: u32 = 1024 * 1024;

/// A component (as wit-component builds one from a core module against a
/// world importing `wasi:keyvalue/store` and `batch`) whose `run` opens the
/// bucket `state`, sets a MiB of memory under the key `k`, then calls
/// `get-many` with `k` named [`REPEATS`] times, for ever. Its `cabi_realloc`
/// hands out one fixed address, so whatever is lowered back is written over
/// the same 2 MiB and nothing it is given can outgrow its 600 pages.
const ASKS_FOR_ONE_KEY_MANY_TIMES: &str = r##"(component
  (type $ty-wasi:keyvalue/store@0.2.0-draft (;0;)
    (instance
      (export (;0;) "bucket" (type (sub resource)))
      (type (;1;) (variant (case "no-such-store") (case "access-denied") (case "other" string)))
      (export (;2;) "error" (type (eq 1)))
      (type (;3;) (borrow 0))
      (type (;4;) (list u8))
      (type (;5;) (result (error 2)))
      (type (;6;) (func (param "self" 3) (param "key" string) (param "value" 4) (result 5)))
      (export (;0;) "[method]bucket.set" (func (type 6)))
      (type (;7;) (own 0))
      (type (;8;) (result 7 (error 2)))
      (type (;9;) (func (param "identifier" string) (result 8)))
      (export (;1;) "open" (func (type 9)))
    )
  )
  (import "wasi:keyvalue/store@0.2.0-draft" (instance $wasi:keyvalue/store@0.2.0-draft (;0;) (type $ty-wasi:keyvalue/store@0.2.0-draft)))
  (alias export $wasi:keyvalue/store@0.2.0-draft "bucket" (type $bucket (;1;)))
  (alias export $wasi:keyvalue/store@0.2.0-draft "error" (type $error (;2;)))
  (type $ty-wasi:keyvalue/batch@0.2.0-draft (;3;)
    (instance
      (alias outer 1 $bucket (type (;0;)))
      (export (;1;) "bucket" (type (eq 0)))
      (alias outer 1 $error (type (;2;)))
      (export (;3;) "error" (type (eq 2)))
      (type (;4;) (borrow 1))
      (type (;5;) (list string))
      (type (;6;) (list u8))
      (type (;7;) (tuple string 6))
      (type (;8;) (option 7))
      (type (;9;) (list 8))
      (type (;10;) (result 9 (error 3)))
      (type (;11;) (func (param "bucket" 4) (param "keys" 5) (result 10)))
      (export (;0;) "get-many" (func (type 11)))
    )
  )
  (import "wasi:keyvalue/batch@0.2.0-draft" (instance $wasi:keyvalue/batch@0.2.0-draft (;1;) (type $ty-wasi:keyvalue/batch@0.2.0-draft)))
  (core module $main (;0;)
    (type (;0;) (func (param i32 i32 i32)))
    (type (;1;) (func (param i32 i32 i32 i32 i32 i32)))
    (type (;2;) (func (param i32 i32 i32 i32)))
    (type (;3;) (func (param i32 i32 i32 i32) (result i32)))
    (type (;4;) (func (result i32)))
    (import "wasi:keyvalue/store@0.2.0-draft" "open" (func $open (;0;) (type 0)))
    (import "wasi:keyvalue/store@0.2.0-draft" "[method]bucket.set" (func $set (;1;) (type 1)))
    (import "wasi:keyvalue/batch@0.2.0-draft" "get-many" (func $get_many (;2;) (type 2)))
    (memory (;0;) 600)
    (export "memory" (memory 0))
    (export "cabi_realloc" (func 3))
    (export "run" (func 4))
    (func (;3;) (type 3) (param i32 i32 i32 i32) (result i32)
      i32.const 2097152
    )
    (func (;4;) (type 4) (result i32)
      (local $i i32) (local $bucket i32)
      ;; open("state") -> result<bucket, error>, written at 256: the
      ;; handle at 260 when it is ok.
      i32.const 0
      i32.const 5
      i32.const 256
      call $open
      i32.const 260
      i32.load
      local.set $bucket
      ;; set(bucket, "k", memory[1 MiB .. 2 MiB])
      local.get $bucket
      i32.const 16
      i32.const 1
      i32.const 1048576
      i32.const 1048576
      i32.const 256
      call $set
      ;; keys: REPEATS pairs of (ptr 16, len 1) from 4096 on
      loop $fill
        i32.const 4096
        local.get $i
        i32.const 8
        i32.mul
        i32.add
        i32.const 16
        i32.store
        i32.const 4100
        local.get $i
        i32.const 8
        i32.mul
        i32.add
        i32.const 1
        i32.store
        local.get $i
        i32.const 1
        i32.add
        local.set $i
        local.get $i
        i32.const 2048
        i32.lt_u
        br_if $fill
      end
      loop $on
        local.get $bucket
        i32.const 4096
        i32.const 2048
        i32.const 256
        call $get_many
        br $on
      end
      i32.const 256
    )
    (data (;0;) (i32.const 0) "state")
    (data (;1;) (i32.const 16) "k")
  )
  (core module $wit-component-shim-module (;1;)
    (type (;0;) (func (param i32 i32 i32)))
    (type (;1;) (func (param i32 i32 i32 i32 i32 i32)))
    (type (;2;) (func (param i32 i32 i32 i32)))
    (table (;0;) 3 3 funcref)
    (export "0" (func 0))
    (export "1" (func 1))
    (export "2" (func 2))
    (export "$imports" (table 0))
    (func (;0;) (type 0) (param i32 i32 i32)
      local.get 0
      local.get 1
      local.get 2
      i32.const 0
      call_indirect (type 0)
    )
    (func (;1;) (type 1) (param i32 i32 i32 i32 i32 i32)
      local.get 0
      local.get 1
      local.get 2
      local.get 3
      local.get 4
      local.get 5
      i32.const 1
      call_indirect (type 1)
    )
    (func (;2;) (type 2) (param i32 i32 i32 i32)
      local.get 0
      local.get 1
      local.get 2
      local.get 3
      i32.const 2
      call_indirect (type 2)
    )
  )
  (core module $wit-component-fixup (;2;)
    (type (;0;) (func (param i32 i32 i32)))
    (type (;1;) (func (param i32 i32 i32 i32 i32 i32)))
    (type (;2;) (func (param i32 i32 i32 i32)))
    (import "" "0" (func (;0;) (type 0)))
    (import "" "1" (func (;1;) (type 1)))
    (import "" "2" (func (;2;) (type 2)))
    (import "" "$imports" (table (;0;) 3 3 funcref))
    (elem (;0;) (i32.const 0) func 0 1 2)
  )
  (core instance $wit-component-shim-instance (;0;) (instantiate $wit-component-shim-module))
  (alias core export $wit-component-shim-instance "0" (core func $indirect-wasi:keyvalue/store@0.2.0-draft-open (;0;)))
  (alias core export $wit-component-shim-instance "1" (core func $"indirect-wasi:keyvalue/store@0.2.0-draft-[method]bucket.set" (;1;)))
  (core instance $wasi:keyvalue/store@0.2.0-draft (;1;)
    (export "open" (func $indirect-wasi:keyvalue/store@0.2.0-draft-open))
    (export "[method]bucket.set" (func $"indirect-wasi:keyvalue/store@0.2.0-draft-[method]bucket.set"))
  )
  (alias core export $wit-component-shim-instance "2" (core func $indirect-wasi:keyvalue/batch@0.2.0-draft-get-many (;2;)))
  (core instance $wasi:keyvalue/batch@0.2.0-draft (;2;)
    (export "get-many" (func $indirect-wasi:keyvalue/batch@0.2.0-draft-get-many))
  )
  (core instance $main (;3;) (instantiate $main
      (with "wasi:keyvalue/store@0.2.0-draft" (instance $wasi:keyvalue/store@0.2.0-draft))
      (with "wasi:keyvalue/batch@0.2.0-draft" (instance $wasi:keyvalue/batch@0.2.0-draft))
    )
  )
  (alias core export $main "memory" (core memory $memory (;0;)))
  (alias core export $wit-component-shim-instance "$imports" (core table $"shim table" (;0;)))
  (alias export $wasi:keyvalue/store@0.2.0-draft "open" (func $open (;0;)))
  (alias core export $main "cabi_realloc" (core func $realloc (;3;)))
  (core func $"#core-func4 indirect-wasi:keyvalue/store@0.2.0-draft-open" (@name "indirect-wasi:keyvalue/store@0.2.0-draft-open") (;4;) (canon lower (func $open) (memory $memory) (realloc $realloc) string-encoding=utf8))
  (alias export $wasi:keyvalue/store@0.2.0-draft "[method]bucket.set" (func $"[method]bucket.set" (;1;)))
  (core func $"#core-func5 indirect-wasi:keyvalue/store@0.2.0-draft-[method]bucket.set" (@name "indirect-wasi:keyvalue/store@0.2.0-draft-[method]bucket.set") (;5;) (canon lower (func $"[method]bucket.set") (memory $memory) (realloc $realloc) string-encoding=utf8))
  (alias export $wasi:keyvalue/batch@0.2.0-draft "get-many" (func $get-many (;2;)))
  (core func $"#core-func6 indirect-wasi:keyvalue/batch@0.2.0-draft-get-many" (@name "indirect-wasi:keyvalue/batch@0.2.0-draft-get-many") (;6;) (canon lower (func $get-many) (memory $memory) (realloc $realloc) string-encoding=utf8))
  (core instance $fixup-args (;4;)
    (export "$imports" (table $"shim table"))
    (export "0" (func $"#core-func4 indirect-wasi:keyvalue/store@0.2.0-draft-open"))
    (export "1" (func $"#core-func5 indirect-wasi:keyvalue/store@0.2.0-draft-[method]bucket.set"))
    (export "2" (func $"#core-func6 indirect-wasi:keyvalue/batch@0.2.0-draft-get-many"))
  )
  (core instance $fixup (;5;) (instantiate $wit-component-fixup
      (with "" (instance $fixup-args))
    )
  )
  (type (;4;) (result (error string)))
  (type (;5;) (func (result 4)))
  (alias core export $main "run" (core func $run (;7;)))
  (func $run (;3;) (type 5) (canon lift (core func $run) (memory $memory) string-encoding=utf8))
  (export $"#func4 run" (@name "run") (;4;) "run" (func $run))
)"##;

/// A process's resident memory now, in MiB.
fn resident_mib(pid: u32) -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0)
        / 1024.0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_one_call_hands_back_to_an_agent_is_held_to_its_memory_allowance() {
    let component = ASKS_FOR_ONE_KEY_MANY_TIMES;
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
    let began = std::time::Instant::now();
    let mut running = false;
    while !running && began.elapsed() < Duration::from_secs(60) {
        tokio::time::sleep(Duration::from_millis(200)).await;
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

    // The executor's resident memory, sampled often for twenty seconds of
    // the agent running: its peak is what one agent can make the host hold.
    let mut peak = 0.0f64;
    let sampled = std::time::Instant::now();
    while sampled.elapsed() < Duration::from_secs(20) {
        peak = peak.max(resident_mib(pid));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(executor);
    let _ = std::fs::remove_dir_all(&dir);

    let said = said.lock().unwrap().clone();
    let copies = f64::from(REPEATS) * f64::from(VALUE) / (1024.0 * 1024.0);
    let allowance = f64::from(shared::Allowances::modest().memory_mib);
    // The executor itself, its runtime, the compiled component and the
    // agent's whole memory allowance: a generous room, and far short of
    // every copy the call names.
    let room = 768.0 + allowance;
    assert!(
        peak < room,
        "an agent held to {allowance} MiB of memory, whose state holds {} MiB, had the executor \
         resident at {peak:.0} MiB at its peak (room for the executor and the agent: {room:.0} \
         MiB): one get-many naming the key {REPEATS} times would be answered with \
         {copies:.0} MiB.\nThe executor said:\n{}",
        f64::from(VALUE) / (1024.0 * 1024.0),
        said.lines()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
