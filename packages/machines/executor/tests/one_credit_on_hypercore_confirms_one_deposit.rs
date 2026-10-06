//! A deposit into HyperCore is reported done only once HyperCore's ledger
//! shows it. Two deposits of the same amount in flight at once (a move that
//! got no confirmation in time and was asked again, or two asked together)
//! are two deposits: one credit in the ledger says one of them arrived, and
//! must not confirm both.

#[allow(dead_code, unused_imports)]
#[path = "../src/hyperliquid.rs"]
mod half;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use alloy::primitives::Address;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A stand-in for HyperCore's `/info`: the account's ledger holds as many
/// deposits of 650 dollars as `credited` says, each its own entry.
async fn stand_in_venue(credited: Arc<AtomicUsize>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let credited = credited.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 65_536];
                tokio::time::sleep(Duration::from_millis(20)).await;
                let _ = socket.read(&mut buffer).await;
                let ledger: Vec<serde_json::Value> = (0..credited.load(Ordering::SeqCst))
                    .map(|n| {
                        serde_json::json!({"time": 1 + n, "hash": format!("0x{n:064x}"),
                        "delta": {"type": "deposit", "usdc": "650.0"}})
                    })
                    .collect();
                let ledger = serde_json::Value::Array(ledger);
                let body = ledger.to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    format!("http://{address}")
}

#[tokio::test]
async fn one_credit_does_not_confirm_two_deposits_of_the_same_amount() {
    let credited = Arc::new(AtomicUsize::new(0));
    let url = stand_in_venue(credited.clone()).await;
    let api = hyperliquid::Api::at(&url, hyperliquid::Network::Testnet);
    let station = Address::repeat_byte(0x51);

    // Two deposits of 650 dollars leave the station, each watched for from
    // before it was sent; neither has been credited yet.
    let first = half::DepositWatch::start(&api, station, 650_000_000)
        .await
        .unwrap();
    let second = half::DepositWatch::start(&api, station, 650_000_000)
        .await
        .unwrap();

    // HyperCore credits one of them, and never the other.
    credited.store(1, Ordering::SeqCst);

    let within = Duration::from_millis(400);
    let every = Duration::from_millis(50);
    let first_done = first.credited(&api, within, every).await;
    let second_done = second.credited(&api, within, every).await;

    assert!(
        !(first_done && second_done),
        "HyperCore credited one deposit of 650 dollars, and both were reported credited"
    );
    assert!(
        first_done || second_done,
        "the one credit confirms one of them"
    );
}

#[tokio::test]
async fn two_credits_confirm_two_deposits_of_the_same_amount() {
    let credited = Arc::new(AtomicUsize::new(0));
    let url = stand_in_venue(credited.clone()).await;
    let api = hyperliquid::Api::at(&url, hyperliquid::Network::Testnet);
    let station = Address::repeat_byte(0x52);

    let first = half::DepositWatch::start(&api, station, 650_000_000)
        .await
        .unwrap();
    let second = half::DepositWatch::start(&api, station, 650_000_000)
        .await
        .unwrap();
    credited.store(2, Ordering::SeqCst);

    let within = Duration::from_millis(400);
    let every = Duration::from_millis(50);
    assert!(first.credited(&api, within, every).await);
    assert!(second.credited(&api, within, every).await);
    // And a third deposit, watched for now, finds both credits taken.
    let third = half::DepositWatch::start(&api, station, 650_000_000)
        .await
        .unwrap();
    assert!(!third.credited(&api, within, every).await);
}
