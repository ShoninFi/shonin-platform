//! A deploy writes its addresses, then sends its setup in waves,
//! recording each step in the deployment file once its send is seen to land,
//! so that a deploy which stops part-way is carried on by the next run.
//!
//! A send can land without this run seeing it: the receipt isn't read within
//! the client's wait (a minute; a deployer on HyperEVM's big blocks lands
//! about once a minute), or the process stops between the send and the
//! record. The step is then unrecorded and the next run sends it again. A
//! step the contract refuses the second time (a market already listed, an
//! image or a project already proposed) then fails on every run: the deploy
//! never finishes its setup, and the steps after it (the driver, the bridge,
//! the accounts' money) are never sent.
//!
//! Here the node is reached through a stand-in that forwards everything and
//! hides one transaction's receipt: the send that proposes the project. The
//! first run stops at that step, as it says it may. The run after it must
//! finish the deployment.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use serde_json::{Value, json};

/// The contracts a deploy reads from the build's output.
const CONTRACTS: &[&str] = &[
    "BatchCreate2",
    "LocalUsd",
    "LocalVenue",
    "DevAttestationVerifier",
    "AttestationRegistry",
    "Publishing",
    "Prices",
    "StrategyRegistry",
    "ReviewRegistry",
    "AuthorProfiles",
    "LocalDriver",
    "LocalBridge",
    "StationCloses",
    "Station",
    "StationFactory",
    "Closer",
    "Valuations",
    "Claims",
    "Posts",
    "Upkeep",
    "Vault",
    "VaultFactory",
];

struct Anvil(Child);

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A free port, never one of the development stack's own.
fn free_port() -> u16 {
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if !(19000..=19200).contains(&port) {
            return port;
        }
    }
}

fn anvil(chain_id: u64) -> Option<(Anvil, u16)> {
    let port = free_port();
    let child = Command::new("anvil")
        .args([
            "--port",
            &port.to_string(),
            "--chain-id",
            &chain_id.to_string(),
            "--silent",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let anvil = Anvil(child);
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        if started.elapsed() > Duration::from_secs(20) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Some((anvil, port))
}

/// A copy of the built contracts a deploy reads, so another build running
/// beside this test changes nothing under it.
fn artifacts(into: &Path) -> Option<PathBuf> {
    let built = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm/out");
    for name in CONTRACTS {
        let from = built
            .join(format!("{name}.sol"))
            .join(format!("{name}.json"));
        if !from.exists() {
            return None;
        }
        let to = into.join(format!("{name}.sol"));
        std::fs::create_dir_all(&to).unwrap();
        std::fs::copy(&from, to.join(format!("{name}.json"))).unwrap();
    }
    Some(into.to_path_buf())
}

/// What the stand-in node knows.
struct StandIn {
    node: String,
    http: reqwest::Client,
    /// The first four bytes of the call whose send's receipt is hidden.
    selector: String,
    /// The transactions whose receipts are hidden.
    unseen: Mutex<Vec<String>>,
}

async fn forward(State(stand_in): State<Arc<StandIn>>, body: Bytes) -> impl IntoResponse {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let method = request["method"].as_str().unwrap_or_default().to_string();
    if method == "eth_getTransactionReceipt" {
        let hash = request["params"][0]
            .as_str()
            .unwrap_or_default()
            .to_lowercase();
        if stand_in.unseen.lock().unwrap().contains(&hash) {
            let answer = json!({"jsonrpc": "2.0", "id": request["id"], "result": null});
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                answer.to_string(),
            );
        }
    }
    let answer = stand_in
        .http
        .post(&stand_in.node)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    if method == "eth_sendTransaction" {
        let sent = &request["params"][0];
        let data = sent["input"]
            .as_str()
            .or_else(|| sent["data"].as_str())
            .unwrap_or_default()
            .to_lowercase();
        // Only the first such send: every later one is seen as it lands.
        if data.starts_with(&stand_in.selector)
            && let Ok(answer) = serde_json::from_str::<Value>(&answer)
            && let Some(hash) = answer["result"].as_str()
        {
            let mut unseen = stand_in.unseen.lock().unwrap();
            if unseen.is_empty() {
                unseen.push(hash.to_lowercase());
            }
        }
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        answer,
    )
}

/// Serve the stand-in on a port of its own, on a thread that lives as long
/// as the test.
fn stand_in(node: String, selector: String) -> (u16, Arc<StandIn>) {
    let state = Arc::new(StandIn {
        node,
        http: reqwest::Client::new(),
        selector,
        unseen: Mutex::new(Vec::new()),
    });
    let port = free_port();
    let serving = state.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                    .await
                    .unwrap();
                let app = Router::new().route("/", post(forward)).with_state(serving);
                axum::serve(listener, app).await.unwrap();
            });
    });
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(started.elapsed() < Duration::from_secs(20), "no stand-in");
        std::thread::sleep(Duration::from_millis(50));
    }
    (port, state)
}

/// Run the deploy, for at most `limit`, and return whether it succeeded and
/// what it wrote.
fn deploy(chains: &Path, dir: &Path, artifacts: &Path, limit: Duration) -> (bool, String) {
    let log = dir.join("deploy.log");
    let out = std::fs::File::create(&log).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_devnet"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .args(["--network", "local", "--vault-chain", "base"])
        .arg("--chains")
        .arg(chains)
        .arg("--deployments-dir")
        .arg(dir.join("deployments"))
        .arg("--store")
        .arg(dir.join("store"))
        .arg("--artifacts")
        .arg(artifacts)
        .arg("deploy")
        .stdout(out.try_clone().unwrap())
        .stderr(out)
        .spawn()
        .expect("the devnet binary");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    (status.is_some_and(|status| status.success()), said)
}

#[test]
fn a_setup_step_that_landed_unseen_does_not_keep_the_deploy_from_finishing() {
    let dir = std::env::temp_dir().join(format!("shonin-review-setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let Some(artifacts) = artifacts(&dir.join("out")) else {
        eprintln!("the contracts are not built: skipping");
        return;
    };
    let Some((_anvil, node_port)) = anvil(31337) else {
        eprintln!("anvil is not installed: skipping");
        return;
    };
    let selector =
        alloy::hex::encode_prefixed(&alloy::primitives::keccak256("proposeProject(bytes32)")[..4]);
    let (port, stand_in) = stand_in(format!("http://127.0.0.1:{node_port}"), selector);
    let chains = dir.join("chains.toml");
    std::fs::write(
        &chains,
        format!(
            "[chains.\"evm:31337\"]\nlabel = \"a local chain\"\nrpc = \"http://127.0.0.1:{port}\"\n\
             dollar = \"USDC\"\nnative_usd = 2500.0\nconfirmations = 0\n\n\
             [networks.local]\nbase = \"evm:31337\"\n"
        ),
    )
    .unwrap();
    let file = dir.join("deployments").join("evm-31337.json");

    // The first run: every contract lands, and the setup stops at the step
    // whose send landed without its receipt being seen.
    let (finished, said) = deploy(&chains, &dir, &artifacts, Duration::from_secs(240));
    assert_eq!(
        stand_in.unseen.lock().unwrap().len(),
        1,
        "the stand-in hid no send's receipt, so this shows nothing:\n{said}"
    );
    assert!(
        !finished,
        "the first run finished though a send's receipt was never seen:\n{said}"
    );
    let text = std::fs::read_to_string(&file)
        .unwrap_or_else(|_| panic!("the addresses were not written before the setup:\n{said}"));
    let recorded: Value = serde_json::from_str(&text).unwrap();
    let done: Vec<&str> = recorded["setup_done"]
        .as_array()
        .expect("a deploy stopped in its setup records the steps done")
        .iter()
        .filter_map(|step| step.as_str())
        .collect();
    // Steps go out in waves a block can hold, so others may wait beside the
    // hidden one; what matters is that the hidden one is not recorded.
    assert!(
        !done.contains(&"project"),
        "the project was recorded as done though its receipt was never seen: {done:?}"
    );

    // The run after it carries on and finishes: the project is proposed
    // already, and the driver, the bridge and the accounts' money follow.
    let (finished, said) = deploy(&chains, &dir, &artifacts, Duration::from_secs(240));
    let text = std::fs::read_to_string(&file).unwrap();
    let recorded: Value = serde_json::from_str(&text).unwrap();
    assert!(
        finished && recorded.get("setup_done").is_none(),
        "the deploy never finishes its setup once a step landed unseen (steps recorded: {}):\n{said}",
        recorded["setup_done"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
