//! One job in flight per author, and each chain's interrupted jobs are
//! picked up once that chain connects. A job a restart caught
//! mid-way on a chain that is configured but not reachable stays `Building`
//! in the queue; it is not being worked, so it does not count as its
//! author's job in flight, and the same author's paid job on a chain that is
//! connected is taken all the same.
//!
//! Sequence: the builder serves Base (node up) and Robinhood (node down).
//! Its queue holds author A's job 8800 on Robinhood, `Building` when the
//! last run stopped, and A's paid job 8801 on Base, `Queued`. Base is taken
//! on. Job 8801 is taken from the queue (here the reviewer is out of
//! reach, so it goes back in the queue noted as having found the reviewer
//! away, which still shows it was taken).

mod black_box;

use std::time::{Duration, Instant};

use alloy::primitives::Address;
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_caught_on_a_chain_not_connected_does_not_hold_the_author_s_jobs_elsewhere() {
    let reference = black_box::start(black_box::node(0x10, None), &[]).await;
    let chains = std::fs::read_to_string(reference.dir.join("chains.toml")).unwrap();
    let rpc = chains
        .lines()
        .find_map(|line| line.strip_prefix("rpc = "))
        .unwrap()
        .to_string();

    let author = Address::repeat_byte(0x77);
    let mut stuck = black_box::a_job(
        "8800",
        author,
        Some(4),
        JobState::Building,
        time::Duration::minutes(5),
    );
    stuck["job"]["chain"] = serde_json::to_value(shared::Chain::Robinhood).unwrap();
    let paid = black_box::a_job(
        "8801",
        author,
        Some(5),
        JobState::Queued { position: 0 },
        time::Duration::minutes(4),
    );
    let (_, queue) = black_box::queue(vec![stuck, paid]);

    let dir = black_box::scratch();
    let store = dir.join("store");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("queue.json"), queue).unwrap();
    let dead = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    std::fs::write(
        dir.join("chains.toml"),
        format!(
            "[chains.\"evm:31337\"]\nlabel = \"base\"\nrpc = {rpc}\ndollar = \"USDC\"\n\
             confirmations = 0\n\
             [chains.\"evm:31338\"]\nlabel = \"hl\"\nrpc = \"http://127.0.0.1:{dead}\"\n\
             dollar = \"USDC\"\n\
             [chains.\"evm:31339\"]\nlabel = \"rh\"\nrpc = \"http://127.0.0.1:{dead}\"\n\
             dollar = \"USDG\"\n\
             [chains.\"solana:localnet\"]\nlabel = \"sol\"\nrpc = \"http://127.0.0.1:{dead}\"\n\
             dollar = \"USDC\"\n\
             [networks.local]\nbase = \"evm:31337\"\nhyperliquid = \"evm:31338\"\n\
             robinhood = \"evm:31339\"\nsolana = \"solana:localnet\"\n"
        ),
    )
    .unwrap();
    let deployments = dir.join("deployments");
    std::fs::create_dir_all(&deployments).unwrap();
    std::fs::copy(
        reference.dir.join("deployments/evm-31337.json"),
        deployments.join("evm-31337.json"),
    )
    .unwrap();
    let port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let mut child = black_box::without_chain_overrides(&mut std::process::Command::new(env!(
        "CARGO_BIN_EXE_builder"
    )))
    .args(["--network", "local", "--chains"])
    .arg(dir.join("chains.toml"))
    .arg("--deployments-dir")
    .arg(&deployments)
    .arg("--store")
    .arg(&store)
    .arg("--builds")
    .arg(dir.join("builds"))
    .arg("--sdk")
    .arg(black_box::sdk(&dir))
    .arg("--vendor")
    .arg(black_box::vendor(&dir))
    .arg("--jobs")
    .arg(dir.join("jobs"))
    .arg("--reviewer-url")
    .arg(format!("http://127.0.0.1:{dead}"))
    .args(["--port", &port.to_string()])
    .env_remove("SHONIN_DEPLOYMENTS_JSON")
    .env_remove("SHONIN_VAULT_CHAIN")
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .spawn()
    .unwrap();

    // Its state, and whether it was ever taken: one put back because the
    // reviewer did not answer is queued again, noted so.
    let read = || -> Option<(JobState, bool)> {
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.join("queue.json")).ok()?).ok()?;
        let entry = written["jobs"]
            .as_array()?
            .iter()
            .find(|entry| entry["job"]["id"] == "8801")?;
        let state = serde_json::from_value(entry["job"]["state"].clone()).ok()?;
        let taken = !matches!(state, JobState::Queued { .. })
            || entry["reviewer_away"].as_u64().unwrap_or(0) > 0;
        Some((state, taken))
    };
    let started = Instant::now();
    let mut state = read();
    while started.elapsed() < Duration::from_secs(25) {
        state = read();
        if state.as_ref().is_some_and(|(_, taken)| *taken) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        state.as_ref().is_some_and(|(_, taken)| *taken),
        "author A's paid Base job was never taken from the queue: their job caught mid-build on \
         Robinhood, a chain not connected, counts as their job in flight (state {state:?})"
    );
}
