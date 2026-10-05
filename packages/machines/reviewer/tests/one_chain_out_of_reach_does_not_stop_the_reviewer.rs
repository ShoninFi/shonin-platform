//! One chain doesn't hold up the others: each service serves a chain as it
//! connects and keeps retrying any that doesn't. The reviewer opens at
//! once, whatever its chains are doing, and a chain whose node does not
//! answer is tried again in the background, for as long as it takes, rather
//! than stopping the reviewer once connecting gives up on it (after about two
//! minutes). Asked for a review on that chain meanwhile, it answers 503,
//! which the build service takes for "ask again later": the job waits in the
//! queue, its $1 kept.
//!
//! Here the reviewer serves one vault chain whose node does not answer
//! (nothing listens on its port).

use std::time::{Duration, Instant};

/// A port nothing listens on.
fn a_free_port() -> u16 {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    probe.local_addr().unwrap().port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_chain_out_of_reach_does_not_stop_the_reviewer() {
    let dir = std::env::temp_dir().join(format!(
        "shonin-reviewer-out-of-reach-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("deployments")).unwrap();
    std::fs::write(
        dir.join("chains.toml"),
        format!(
            "[chains.\"evm:31337\"]\nlabel = \"out of reach\"\nrpc = \"http://127.0.0.1:{}\"\n\
             dollar = \"USDC\"\n[networks.local]\nbase = \"evm:31337\"\n",
            a_free_port()
        ),
    )
    .unwrap();
    let port = a_free_port();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_reviewer"))
        .args(["--network", "local", "--vault-chain", "base", "--chains"])
        .arg(dir.join("chains.toml"))
        .arg("--deployments-dir")
        .arg(dir.join("deployments"))
        .arg("--store")
        .arg(dir.join("store"))
        .args(["--port", &port.to_string()])
        .env_remove("SHONIN_DEPLOYMENTS_JSON")
        .env_remove("SHONIN_REVIEW_MODEL")
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let url = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();

    // It opens while its one chain is still out of reach.
    let started = Instant::now();
    let opened = loop {
        if http.get(format!("{url}/health")).send().await.is_ok() {
            break true;
        }
        if started.elapsed() > Duration::from_secs(30) || child.try_wait().unwrap().is_some() {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let asked = if opened {
        http.post(format!("{url}/base/reviews"))
            .json(&shared::ReviewRequest {
                strategy_id: 5,
                archive_base64: String::new(),
                sealed: false,
                manifest_path: "shonin.toml".to_string(),
            })
            .send()
            .await
            .ok()
            .map(|reply| reply.status().as_u16())
    } else {
        None
    };

    // And stays up past the two minutes connecting would once give up after.
    let mut exited = None;
    while opened && started.elapsed() < Duration::from_secs(135) {
        if let Some(status) = child.try_wait().unwrap() {
            exited = Some((status, started.elapsed()));
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        opened,
        "the reviewer did not open while one chain was out of reach"
    );
    assert_eq!(
        asked,
        Some(503),
        "a review on a chain not connected yet is not answered 503 (ask again later)"
    );
    assert!(
        exited.is_none(),
        "the reviewer gave up and exited ({exited:?}) because one chain was out of reach"
    );
}
