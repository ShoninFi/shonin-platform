//! One vault chain whose node doesn't answer doesn't keep the build service
//! from the chains that do: each chain is taken on as it connects, and a chain not yet connected has
//! its uploads turned away for now (503) rather than every chain's.
//!
//! Sequence: the network names Base (its node up) and Hyperliquid and
//! Robinhood (their nodes down). The builder is started serving every chain
//! of the network, as `task builder` starts it. It opens within a few
//! seconds and takes a Base upload; a Hyperliquid upload is turned away as
//! not yet possible.

mod black_box;

use std::time::{Duration, Instant};

use alloy::primitives::Address;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_chain_down_does_not_keep_the_builder_from_the_others() {
    // A builder of the black box, only for its stand-in node and reviewer.
    let reference = black_box::start(black_box::node(0x10, None), &[]).await;
    let chains = std::fs::read_to_string(reference.dir.join("chains.toml")).unwrap();
    let rpc = chains
        .lines()
        .find_map(|line| line.strip_prefix("rpc = "))
        .unwrap()
        .to_string();
    let reviewer = "http://127.0.0.1:9".to_string();

    let dir = black_box::scratch();
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
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_builder"))
        .args(["--network", "local", "--chains"])
        .arg(dir.join("chains.toml"))
        .arg("--deployments-dir")
        .arg(&deployments)
        .arg("--store")
        .arg(dir.join("store"))
        .arg("--builds")
        .arg(dir.join("builds"))
        .arg("--sdk")
        .arg(dir.join("sdk"))
        .arg("--vendor")
        .arg(black_box::vendor(&dir))
        .arg("--jobs")
        .arg(dir.join("jobs"))
        .arg("--reviewer-url")
        .arg(&reviewer)
        .args(["--port", &port.to_string()])
        .env_remove("SHONIN_DEPLOYMENTS_JSON")
        .env_remove("SHONIN_VAULT_CHAIN")
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let url = format!("http://127.0.0.1:{port}");
    let started = Instant::now();
    let mut opened = false;
    while started.elapsed() < Duration::from_secs(20) {
        if reqwest::get(format!("{url}/health")).await.is_ok() {
            opened = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let send = |chain: &'static str| {
        let url = url.clone();
        async move {
            reqwest::Client::new()
                .post(format!("{url}/{chain}/uploads"))
                .json(&shared::Upload {
                    author: format!("{:?}", Address::repeat_byte(0x77)),
                    archive_base64: shared::base64::encode(&black_box::project(chain)),
                    sealed: false,
                    package: None,
                })
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    // Base is taken on once its key is blessed there, a moment after the
    // builder opens.
    let mut uploaded = None;
    while opened && started.elapsed() < Duration::from_secs(20) {
        let status = send("base").await;
        uploaded = Some(status);
        if status != 503 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let elsewhere = if opened {
        Some(send("hyperliquid").await)
    } else {
        None
    };
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        opened,
        "with Hyperliquid's and Robinhood's nodes down, the builder never opened for Base"
    );
    assert_eq!(uploaded, Some(200));
    assert_eq!(elsewhere, Some(503));
}
