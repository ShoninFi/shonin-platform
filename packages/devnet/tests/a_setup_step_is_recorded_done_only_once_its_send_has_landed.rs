//! A deploy's setup looks for each step on the chain before sending it, and
//! records a step it finds done without sending it. Once every step is
//! recorded the deployment file says the setup is finished, and no later run
//! sends anything.
//!
//! The look must be at the latest block, not at the node's `pending` state
//! (the head's state with the transactions waiting in the node's pool
//! applied). A send of the step that outlasted the run that made it (the run
//! waits a minute for the receipt and stops) is waiting in that pool; read at
//! `pending`, the next run would find the step "done on the chain" while no
//! block holds it, record it, and write the file as finished. A send waiting
//! in a pool is not a send that landed: the node drops it when its
//! pool evicts it or it restarts, and then nothing ever sends the step again.
//!
//! Here the node is reached through a stand-in that forwards everything. It
//! holds the last step's send (the admin's starting money) back from landing:
//! the node stops making blocks, as a chain that makes one a minute does
//! between two of them. The first run stops at that step, as it says it may.
//! While the second run is looking, just after the node has answered its
//! read of the admin's money, the node drops the waiting send; the chain then
//! goes on. A deployment recorded as set up must have handed the admin its
//! money, however many runs that takes.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// What the setup hands each account: a million of the deployment's dollar.
const STARTING_BALANCE: u128 = 1_000_000_000_000;
/// The setup's last step is the fourth account's money.
const LAST_MINT: usize = 4;
/// `balanceOf(address)`.
const BALANCE_OF: &str = "0x70a08231";

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
    /// The first four bytes of a mint's call.
    selector: String,
    /// How many mints were sent through it.
    mints: Mutex<usize>,
    /// The last step's send, while it waits to land.
    waiting: Mutex<Option<String>>,
    /// Whether the node drops the waiting send once it has next answered a
    /// read of an account's money.
    drops_after_the_next_look: AtomicBool,
}

impl StandIn {
    async fn ask_node(&self, method: &str, params: Value) -> Value {
        let answer: Value = self
            .http
            .post(&self.node)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        answer["result"].clone()
    }

    /// The node drops the send still waiting, if one is, and the chain goes
    /// on.
    async fn drop_the_waiting_send(&self) {
        let waiting = self.waiting.lock().unwrap().take();
        if let Some(hash) = waiting {
            self.ask_node("anvil_dropTransaction", json!([hash])).await;
        }
        self.ask_node("evm_setAutomine", json!([true])).await;
        self.ask_node("evm_mine", json!([])).await;
    }
}

async fn forward(State(stand_in): State<Arc<StandIn>>, body: Bytes) -> impl IntoResponse {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let method = request["method"].as_str().unwrap_or_default().to_string();
    let sent = &request["params"][0];
    let data = sent["input"]
        .as_str()
        .or_else(|| sent["data"].as_str())
        .unwrap_or_default()
        .to_lowercase();
    let mut nth_mint = None;
    if method == "eth_sendTransaction" && data.starts_with(&stand_in.selector) {
        let mut mints = stand_in.mints.lock().unwrap();
        *mints += 1;
        nth_mint = Some(*mints);
    }
    // The last step's send is taken into a chain that makes no block for a
    // while.
    if nth_mint == Some(LAST_MINT) {
        stand_in.ask_node("evm_setAutomine", json!([false])).await;
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
    if nth_mint == Some(LAST_MINT)
        && let Ok(answer) = serde_json::from_str::<Value>(&answer)
        && let Some(hash) = answer["result"].as_str()
    {
        *stand_in.waiting.lock().unwrap() = Some(hash.to_lowercase());
    }
    // The node has answered a look at an account's money: it now drops the
    // send that was waiting.
    if method == "eth_call"
        && data.starts_with(BALANCE_OF)
        && stand_in.waiting.lock().unwrap().is_some()
        && stand_in.drops_after_the_next_look.load(Ordering::SeqCst)
    {
        stand_in.drop_the_waiting_send().await;
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
        mints: Mutex::new(0),
        waiting: Mutex::new(None),
        drops_after_the_next_look: AtomicBool::new(false),
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
fn a_setup_step_is_recorded_done_only_once_its_send_has_landed() {
    let dir = std::env::temp_dir().join(format!("shonin-waiting-step-{}", std::process::id()));
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
        alloy::hex::encode_prefixed(&alloy::primitives::keccak256("mint(address,uint256)")[..4]);
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
    let runtime = tokio::runtime::Runtime::new().unwrap();

    // The first run: every contract lands, and the setup stops at its last
    // step, whose send is still waiting when the run's wait for it ends.
    let (finished, said) = deploy(&chains, &dir, &artifacts, Duration::from_secs(240));
    let waiting = stand_in.waiting.lock().unwrap().clone();
    let Some(waiting) = waiting else {
        panic!("the last step's send was not held back, so this shows nothing:\n{said}");
    };
    assert!(
        !finished,
        "the first run finished though its last send never landed:\n{said}"
    );
    let text = std::fs::read_to_string(&file)
        .unwrap_or_else(|_| panic!("the addresses were not written before the setup:\n{said}"));
    let recorded: Value = serde_json::from_str(&text).unwrap();
    let admin = recorded["roles"]["admin"].as_str().unwrap().to_string();
    let usd = recorded["usd"].as_str().unwrap().to_string();
    // Steps are sent in waves a block can hold, so others may wait beside
    // the held one; what matters is that the held step is not recorded.
    let done = recorded["setup_done"]
        .as_array()
        .unwrap_or_else(|| panic!("the setup was recorded as finished:\n{said}"));
    assert!(
        !done.iter().any(|step| step == "money admin"),
        "the admin's money was recorded as done while its send was waiting: {done:?}"
    );
    let receipt =
        runtime.block_on(stand_in.ask_node("eth_getTransactionReceipt", json!([waiting])));
    assert!(
        receipt.is_null(),
        "the last step's send landed, so this shows nothing: {receipt}"
    );

    // The run after it, during which the node drops the waiting send, and
    // one more for a deploy that stopped to be run again.
    stand_in
        .drops_after_the_next_look
        .store(true, Ordering::SeqCst);
    let (_, second) = deploy(&chains, &dir, &artifacts, Duration::from_secs(420));
    runtime.block_on(stand_in.drop_the_waiting_send());
    let (_, third) = deploy(&chains, &dir, &artifacts, Duration::from_secs(240));

    let recorded: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let balance_of = format!(
        "{BALANCE_OF}{:0>64}",
        admin.trim_start_matches("0x").to_lowercase()
    );
    let held = runtime.block_on(stand_in.ask_node(
        "eth_call",
        json!([{"to": usd, "data": balance_of}, "latest"]),
    ));
    let held = u128::from_str_radix(held.as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
    assert!(
        recorded.get("setup_done").is_some() || held >= STARTING_BALANCE,
        "the deployment file says the setup is finished, and the admin holds {held} of its \
         starting {STARTING_BALANCE}: the step was recorded as done while its send was waiting in \
         the node's pool, the node dropped that send, and no run sends it again.\n\
         The second run:\n{second}\nThe third run:\n{third}"
    );
    assert_eq!(
        held, STARTING_BALANCE,
        "the admin's starting money, once every run has ended.\n\
         The second run:\n{second}\nThe third run:\n{third}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
