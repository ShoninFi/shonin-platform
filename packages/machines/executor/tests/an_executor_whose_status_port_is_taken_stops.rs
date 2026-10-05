//! An executor that can't bind its status port stops, rather than run on
//! with every signed total unserved, while whatever holds the port answers
//! for it: the keeper, the author's withdraw screen and the tests read a
//! vault's signed total only there, and another executor on the port would
//! answer with its own.
//!
//! One executor binds a port the system picks; a second is told that port.

mod black_box;

use std::time::{Duration, Instant};

const RUNS: &str = r#"(component
    (core module $m
        (memory (export "memory") 1)
        (func (export "run") (result i32)
            (loop $on (br $on))
            (i32.const 0)))
    (core instance $i (instantiate $m))
    (func $run async (result (result (error string)))
        (canon lift (core func $i "run") (memory (core memory $i "memory"))))
    (export "run" (func $run)))"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_executor_whose_status_port_is_taken_stops() {
    let first = black_box::start(RUNS, black_box::Script::Approved).await;

    let mut second = std::process::Command::new(env!("CARGO_BIN_EXE_executor"))
        .args(["--network", "local", "--vault-chain", "base", "--chains"])
        .arg(first.dir.join("chains.toml"))
        .arg("--deployments-dir")
        .arg(first.dir.join("deployments"))
        .arg("--store")
        .arg(first.dir.join("store"))
        .args(["--api-url", "http://127.0.0.1:9"])
        .args(["--port", &first.port.to_string()])
        .env_remove("SHONIN_DEPLOYMENTS_JSON")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let began = Instant::now();
    let stopped = loop {
        if let Some(status) = second.try_wait().unwrap() {
            break Some(status);
        }
        if began.elapsed() > Duration::from_secs(20) {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let _ = second.kill();
    let _ = second.wait();
    drop(first);
    match stopped {
        Some(status) => assert!(
            !status.success(),
            "the executor stopped as if all were well ({status})"
        ),
        None => panic!(
            "an executor told a port another executor holds was still running 20 s on, \
             its signed totals unserved and the port answering for the other"
        ),
    }
}
