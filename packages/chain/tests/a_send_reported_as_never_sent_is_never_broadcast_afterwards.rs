//! A send the caller is told was never sent (`NeverSent`, so
//! `may_still_land` is false) never reaches the node afterwards: the caller
//! may well send again on the strength of it, and two would land.
//!
//! The caller and the broadcast task work to one deadline, and giving up and
//! broadcasting each claim the send's stage, so a broadcast either begins
//! before the caller gives up (and the send may still land) or not at all.
//! Here the broadcast task is first run late, as a busy runtime would run
//! it: another task moves a paused clock on before it is first polled, and
//! filling the transaction in is slow.
//!
//! Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// What the node has been sent, and how long it takes over a gas estimate.
#[derive(Default)]
struct Slow {
    sent: Vec<B256>,
    estimate_delay: Option<Duration>,
}

fn answer(node: &Mutex<Slow>, request: &Value) -> (Value, Option<Duration>) {
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or_default();
    let mut node = node.lock().unwrap();
    let mut hold = None;
    let result = match method {
        "eth_chainId" => json!("0x7a69"),
        "eth_blockNumber" => json!("0x10"),
        "eth_getBlockByNumber" => {
            let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
            block.header.inner.number = 16;
            block.header.inner.timestamp = 1_700_000_000;
            block.header.inner.base_fee_per_gas = Some(1_000_000_000);
            serde_json::to_value(block).unwrap()
        }
        "eth_gasPrice" | "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
        "eth_feeHistory" => json!({
            "oldestBlock": "0x1",
            "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
            "gasUsedRatio": [0.5],
            "reward": [["0x3b9aca00"]],
        }),
        "eth_estimateGas" => {
            hold = node.estimate_delay;
            json!("0x5208")
        }
        // The connect-time check of whether a read at a past block is
        // answered as of that block: answered honestly.
        "eth_call" => {
            let block = request["params"][1].as_str().unwrap_or("0x0");
            let number = u64::from_str_radix(block.trim_start_matches("0x"), 16).unwrap_or(0);
            json!(B256::from(alloy::primitives::U256::from(number)))
        }
        "eth_getTransactionCount" => json!(format!("0x{:x}", node.sent.len())),
        "eth_sendRawTransaction" => {
            let raw = request["params"][0].as_str().unwrap();
            let hash = alloy::primitives::keccak256(alloy::hex::decode(raw).unwrap());
            node.sent.push(hash);
            json!(hash)
        }
        "eth_getTransactionReceipt" => Value::Null,
        other => panic!("the stand-in node was asked {other}"),
    };
    (json!({"jsonrpc": "2.0", "id": id, "result": result}), hold)
}

async fn start(node: Arc<Mutex<Slow>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let node = node.clone();
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                loop {
                    let header_end = loop {
                        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                            break at + 4;
                        }
                        let mut chunk = [0u8; 4096];
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    while buffer.len() < header_end + length {
                        let mut chunk = [0u8; 4096];
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let body: Value =
                        serde_json::from_slice(&buffer[header_end..header_end + length]).unwrap();
                    buffer.drain(..header_end + length);
                    let (reply, hold) = match &body {
                        Value::Array(batch) => {
                            let mut hold = None;
                            let answers: Vec<Value> = batch
                                .iter()
                                .map(|one| {
                                    let (value, wait) = answer(&node, one);
                                    hold = hold.or(wait);
                                    value
                                })
                                .collect();
                            (Value::Array(answers), hold)
                        }
                        one => answer(&node, one),
                    };
                    if let Some(wait) = hold {
                        tokio::time::sleep(wait).await;
                    }
                    let text = reply.to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{text}",
                        text.len()
                    );
                    if socket.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    format!("http://{address}")
}

#[tokio::test]
async fn a_send_reported_as_never_sent_is_never_broadcast_afterwards() {
    let node = Arc::new(Mutex::new(Slow::default()));
    let url = start(node.clone()).await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    let key = client.with_key(PrivateKeySigner::random()).unwrap();

    // Filling the transaction in takes the node 15 s (a slow gas estimate,
    // well inside a request's 30 s timeout).
    node.lock().unwrap().estimate_delay = Some(Duration::from_secs(15));

    // A task that moves the clock on 50 s just before the broadcast task is
    // first polled: the runtime was busy for that long after the spawn.
    let go = Arc::new(tokio::sync::Notify::new());
    let busy = {
        let go = go.clone();
        tokio::spawn(async move {
            go.notified().await;
            tokio::time::advance(Duration::from_secs(50)).await;
        })
    };
    tokio::task::yield_now().await;
    tokio::time::pause();
    go.notify_one();

    let error = key
        .send(
            Address::repeat_byte(0x22),
            IERC20::approveCall {
                spender: Address::repeat_byte(0x33),
                value: U256::from(1u64),
            },
        )
        .await
        .expect_err("the caller gives up 60 s after the spawn");
    busy.await.unwrap();
    let told_never_sent = !chain::may_still_land(&error);

    // Let the broadcast task carry on for as long as it would.
    for _ in 0..400 {
        if !node.lock().unwrap().sent.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let broadcast = node.lock().unwrap().sent.len();

    assert!(
        !(told_never_sent && broadcast > 0),
        "the caller was told nothing was sent ({error:#}), so it may send again, \
         and then the node was handed the transaction anyway ({broadcast} broadcast)"
    );
}
