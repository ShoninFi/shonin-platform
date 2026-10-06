//! The venue answers some actions it took with a plain "ok" that carries no
//! per-order status (`{"status": "ok", "response": {"type": "default"}}`):
//! a leverage change, a scheduled cancel, and, by the venue's own tools'
//! history, a single `modify`. An agent whose modification the venue took
//! hears it was taken, never "unknown": "unknown" says the answer was lost
//! and the action may or may not have been done, and there is nothing in
//! the venue's record the driver reads back for a modification, so it would
//! never hear anything else. A placement's answer without a status is still
//! looked up.
//!
//! (What the live venue answers a single `modify` with could not be checked
//! here; the stand-in answers as described above.)

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use serde_json::{Value, json};
use shonin_hyperliquid::agent::{Allowlist, Outcome, Trader};
use shonin_hyperliquid::payload::{Action, Order, OrderRef, Receipt, Side, Tif};
use shonin_hyperliquid::{Api, Markets, Network};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// One whole HTTP request: its path and its body, as JSON.
async fn request(socket: &mut tokio::net::TcpStream) -> Option<(String, Value)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buffer).to_string();
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length: usize = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())?
                })
                .unwrap_or(0);
            if body.len() >= length {
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                return Some((path, serde_json::from_str(&body[..length]).ok()?));
            }
        }
    }
}

/// A stand-in venue that takes every action and answers it with a plain
/// "ok"; its `/info` knows no order.
async fn venue() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let Some((path, _)) = request(&mut socket).await else {
                    return;
                };
                let answer = if path.ends_with("/exchange") {
                    json!({"status": "ok", "response": {"type": "default"}})
                } else {
                    json!({"status": "unknownOid"})
                };
                let answer = answer.to_string();
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    format!("http://{address}")
}

fn trader(url: &str) -> Trader {
    let meta = json!({"universe": [
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
    ]});
    let spot = json!({
        "tokens": [
            {"name": "USDC", "index": 0, "szDecimals": 8, "weiDecimals": 8, "tokenId": "0xaa"}
        ],
        "universe": []
    });
    let markets = Markets::from_meta(&meta, &spot).unwrap();
    let allowlist = Allowlist::from_driver(Address::repeat_byte(0x55), markets, &[0], &[]).unwrap();
    Trader::new(
        Api::at(url, Network::Testnet),
        PrivateKeySigner::random(),
        allowlist,
    )
}

#[tokio::test]
async fn a_modification_the_venue_answered_with_a_plain_ok_is_reported_taken() {
    let url = venue().await;
    let trader = trader(&url);

    // The stand-in answers a leverage change the same way, and that is done.
    let leverage = trader
        .outcome(&Action::Leverage {
            market: "ETH".to_string(),
            leverage: 3,
        })
        .await;
    assert!(
        matches!(leverage, Outcome::Accepted(_)),
        "a plain ok to a leverage change: {leverage:?}"
    );

    let modified = trader
        .outcome(&Action::Modify {
            order_ref: OrderRef::Venue(77),
            order: Order {
                market: "ETH".to_string(),
                side: Side::Buy,
                size: "0.5".parse().unwrap(),
                limit_price: "2700".parse().unwrap(),
                tif: Tif::Gtc,
                reduce_only: false,
                client_id: None,
            },
        })
        .await;
    assert_eq!(modified, Outcome::Accepted(Receipt::Done));

    // A placement answered the same way says nothing of what the order did:
    // it is looked up, and the stand-in's record knows no such order.
    let placed = trader
        .outcome(&Action::Place(Order {
            market: "ETH".to_string(),
            side: Side::Buy,
            size: "0.5".parse().unwrap(),
            limit_price: "2700".parse().unwrap(),
            tif: Tif::Gtc,
            reduce_only: false,
            client_id: None,
        }))
        .await;
    assert!(matches!(placed, Outcome::Unknown(_)), "{placed:?}");
}
