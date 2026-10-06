//! An agent sizes its orders against the account the driver reads for it. An
//! answer from the venue that isn't the account (an error body, `null`), or
//! one with a position whose size can't be read, must come back as an error:
//! read as an account worth nothing with no positions, an agent that trusts
//! it opens again what it already holds.

use alloy::primitives::Address;
use serde_json::{Value, json};
use shonin_hyperliquid::events::account_state;
use shonin_hyperliquid::{Api, Markets, Network};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// One whole HTTP request's body, as JSON.
async fn request(socket: &mut tokio::net::TcpStream) -> Option<Value> {
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
                return serde_json::from_str(&body[..length]).ok();
            }
        }
    }
}

/// A stand-in venue whose `/info` answers the perp account with `perp`, the
/// spot account with no balances and the open orders with none.
async fn venue(perp: Value) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let perp = perp.clone();
            tokio::spawn(async move {
                let Some(body) = request(&mut socket).await else {
                    return;
                };
                let answer = match body["type"].as_str() {
                    Some("clearinghouseState") => perp,
                    Some("spotClearinghouseState") => json!({"balances": []}),
                    _ => json!([]),
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

fn markets() -> Markets {
    let meta = json!({"universe": [
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
    ]});
    let spot = json!({
        "tokens": [
            {"name": "USDC", "index": 0, "szDecimals": 8, "weiDecimals": 8, "tokenId": "0xaa"}
        ],
        "universe": []
    });
    Markets::from_meta(&meta, &spot).unwrap()
}

async fn read(perp: Value) -> anyhow::Result<shonin_hyperliquid::payload::AccountState> {
    let url = venue(perp).await;
    let api = Api::at(&url, Network::Testnet);
    account_state(&api, &markets(), Address::repeat_byte(0x51)).await
}

#[tokio::test]
async fn an_answer_that_is_not_the_account_is_an_error_never_an_empty_account() {
    for not_the_account in [json!(null), json!({"error": "try again later"})] {
        let state = read(not_the_account.clone()).await;
        assert!(
            state.is_err(),
            "the venue answered {not_the_account}, and the agent was handed an account worth {} with {} positions",
            state.as_ref().unwrap().account_value,
            state.as_ref().unwrap().positions.len()
        );
    }
}

#[tokio::test]
async fn a_position_whose_size_cannot_be_read_is_an_error_never_left_out() {
    // The venue writes sizes as strings; here one comes back as a number.
    let state = read(json!({
        "marginSummary": {"accountValue": "100.5"},
        "withdrawable": "80",
        "assetPositions": [
            {"position": {"coin": "ETH", "szi": -0.1, "entryPx": "2700",
                "leverage": {"type": "cross", "value": 3}}}
        ]
    }))
    .await;
    assert!(
        state.is_err(),
        "the account holds a position the driver couldn't read, and the agent was handed {} positions",
        state.as_ref().unwrap().positions.len()
    );
}
