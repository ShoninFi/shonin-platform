//! A stand-in node for black-box tests of the reviewer binary, and a way to
//! start the reviewer against it with a `claude` of the test's own on its
//! path.
//!
//! The node holds enough of the attestation registry to bless the
//! reviewer's key, and answers every other read with a refusal.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256, U256};
use alloy::sol_types::SolCall;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use chain::bindings::attestation::{IAttestationRegistry, ImageKind, KeyInfo};
use serde_json::{Value, json};

const REVIEWER_KEY: Address = Address::repeat_byte(0xc1);

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub struct Node {
    pub head: AtomicU64,
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
        "from": format!("{REVIEWER_KEY:?}"),
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
        "eth_sign" => json!(format!("0x{}{}", "11".repeat(64), "1b")),
        "eth_sendTransaction" => json!(format!("0x{}", "33".repeat(32))),
        "eth_getTransactionReceipt" => receipt(request["params"][0].as_str().unwrap_or_default()),
        "eth_getLogs" => json!([]),
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
                        kind: ImageKind::Reviewer,
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

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

static SCRATCH: AtomicU64 = AtomicU64::new(0);

pub fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-reviewer-box-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A port nothing listens on.
pub fn a_free_port() -> u16 {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    probe.local_addr().unwrap().port()
}

/// A `claude` of the test's own, on a path of its own, running `script`
/// (a shell script's body) in place of the model. Where the path is.
pub fn a_claude(script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let bin = scratch().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let claude = bin.join("claude");
    std::fs::write(&claude, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// A project the reviewer reads: one agent, on base, trading through the
/// local driver.
pub fn project(tag: &str) -> Vec<u8> {
    let root = scratch().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"agent\"\n").unwrap();
    std::fs::write(
        root.join("Cargo.lock"),
        "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\n\
         name = \"agent\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
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

pub struct Reviewer {
    pub child: std::process::Child,
    pub url: String,
    pub dir: PathBuf,
    pub said: Arc<std::sync::Mutex<String>>,
}

impl Drop for Reviewer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Reviewer {
    pub fn store(&self) -> PathBuf {
        self.dir.join("store")
    }

    /// Ask for a review of `archive` on base, as the build service does.
    pub async fn review(&self, strategy_id: u64, archive: &[u8]) -> (u16, String) {
        let reply = reqwest::Client::new()
            .post(format!("{}/base/reviews", self.url))
            .json(&shared::ReviewRequest {
                strategy_id,
                archive_base64: shared::base64::encode(archive),
                sealed: false,
                manifest_path: "shonin.toml".to_string(),
            })
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .unwrap();
        (reply.status().as_u16(), reply.text().await.unwrap())
    }

    /// Wait up to `within` for the reviewer to have said `what`.
    pub async fn wait_until_said(&self, what: &str, within: Duration) -> bool {
        let started = Instant::now();
        while !self.said.lock().unwrap().contains(what) {
            if started.elapsed() > within {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        true
    }
}

/// How to start the reviewer.
#[derive(Default)]
pub struct Options<'a> {
    /// A directory put first on the reviewer's path (holding a `claude`).
    pub path_first: Option<&'a Path>,
    /// The review model to name, if any.
    pub model: Option<&'a str>,
    /// Environment the reviewer is given besides.
    pub env: Vec<(&'a str, &'a str)>,
    /// Files put in the store before it starts, by name and body.
    pub store_files: Vec<(&'a str, &'a str)>,
    /// Whether the chain's node is there: without one, nothing listens.
    pub node: bool,
    /// Arguments the reviewer is given besides.
    pub args: Vec<&'a str>,
}

/// Start the reviewer binary against a stand-in node (or against nothing),
/// without waiting for it to open.
pub async fn spawn(options: Options<'_>) -> Reviewer {
    let dir = scratch();
    let store = dir.join("store");
    std::fs::create_dir_all(&store).unwrap();
    for (name, body) in &options.store_files {
        std::fs::write(store.join(name), body).unwrap();
    }
    let node_at = if options.node {
        let node = Arc::new(Node { head: 0x10.into() });
        serve(Router::new().route("/", post(rpc)).with_state(node)).await
    } else {
        SocketAddr::from(([127, 0, 0, 1], a_free_port()))
    };
    std::fs::write(
        dir.join("chains.toml"),
        format!(
            "[chains.\"evm:31337\"]\nlabel = \"stand-in\"\nrpc = \"http://{node_at}\"\n\
             dollar = \"USDC\"\nconfirmations = 0\n[networks.local]\nbase = \"evm:31337\"\n"
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
    roles.insert("reviewer".into(), json!(format!("{REVIEWER_KEY:?}")));
    deployments["roles"] = roles.into();
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

    let port = a_free_port();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_reviewer"));
    command
        .args(["--network", "local", "--vault-chain", "base", "--chains"])
        .arg(dir.join("chains.toml"))
        .arg("--deployments-dir")
        .arg(&deployments_dir)
        .arg("--store")
        .arg(&store)
        .args(["--port", &port.to_string()])
        .args(&options.args)
        .env_remove("SHONIN_DEPLOYMENTS_JSON")
        .env_remove("SHONIN_REVIEW_MODEL")
        .env_remove("SHONIN_CALIBRATOR_KEY")
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "info")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(first) = options.path_first {
        command.env(
            "PATH",
            format!(
                "{}:{}",
                first.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
    }
    if let Some(model) = options.model {
        command.env("SHONIN_REVIEW_MODEL", model);
    }
    for (name, value) in &options.env {
        command.env(name, value);
    }
    let mut child = command.spawn().unwrap();
    let said = Arc::new(std::sync::Mutex::new(String::new()));
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
    Reviewer {
        child,
        url: format!("http://127.0.0.1:{port}"),
        dir,
        said,
    }
}

/// [`spawn`], then wait for the reviewer to open and, with a node, to be
/// reviewing on its chain.
pub async fn start(options: Options<'_>) -> Reviewer {
    let with_node = options.node;
    let reviewer = spawn(options).await;
    let http = reqwest::Client::new();
    let started = Instant::now();
    while http
        .get(format!("{}/health", reviewer.url))
        .send()
        .await
        .is_err()
    {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the reviewer never opened:\n{}",
            reviewer.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if with_node {
        assert!(
            reviewer
                .wait_until_said("reviewing on this chain", Duration::from_secs(60))
                .await,
            "the reviewer never took its chain on:\n{}",
            reviewer.said.lock().unwrap()
        );
    }
    reviewer
}
