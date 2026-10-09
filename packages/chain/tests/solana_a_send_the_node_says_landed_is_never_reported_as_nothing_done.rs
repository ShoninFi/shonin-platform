//! A send the node says has landed is never reported as one where nothing
//! was done, even when the node keeps refusing its record for rate: the
//! error says it may have landed (`MayStillLand`), so its cost is looked for
//! and billed later, and it doesn't read as the node's rate limit (which
//! says nothing was done).
//!
//! A stand-in node takes the transaction, says it is confirmed, and answers
//! `getTransaction` with 429 every time.

#![cfg(feature = "solana")]

use std::sync::Arc;

use chain::solana::executor::ix_fund;
use chain::solana::{Keypair, MayStillLand, Signer, SolanaClient, rate_limited};
use chain::{ChainConfig, ChainKey, ChainKind};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A node that takes the transaction, says it is confirmed, and answers
/// `getTransaction` with 429 every time.
fn answer(method: &str) -> (u16, Value) {
    match method {
        "getGenesisHash" => (200, json!("stand-in-genesis")),
        "getLatestBlockhash" => (
            200,
            json!({"context": {"slot": 10}, "value": {
                "blockhash": "11111111111111111111111111111111",
                "lastValidBlockHeight": 1_000_000u64,
            }}),
        ),
        "sendTransaction" => (200, json!("5".repeat(87))),
        "getSignatureStatuses" => (
            200,
            json!({"context": {"slot": 11}, "value": [{
                "slot": 11, "confirmations": null, "err": null,
                "confirmationStatus": "confirmed",
            }]}),
        ),
        "getBlockHeight" => (200, json!(10)),
        "getTransaction" => (429, Value::Null),
        other => panic!("the stand-in node was asked {other}"),
    }
}

async fn serve() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 65536];
                let body = loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buffer).to_string();
                    if let Some(split) = text.find("\r\n\r\n") {
                        let length = text[..split]
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if buffer.len() >= split + 4 + length {
                            break buffer[split + 4..split + 4 + length].to_vec();
                        }
                    }
                };
                let request: Value = serde_json::from_slice(&body).unwrap();
                let (status, result) = answer(request["method"].as_str().unwrap());
                let reply = if status == 429 {
                    json!({"jsonrpc": "2.0", "id": request["id"], "error": {"code": 429, "message": "Too many requests for a specific RPC call"}}).to_string()
                } else {
                    json!({"jsonrpc": "2.0", "id": request["id"], "result": result}).to_string()
                };
                let http = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(http.as_bytes()).await;
            });
        }
    });
    url
}

#[tokio::test]
async fn a_send_the_node_says_landed_is_never_reported_as_nothing_done_when_its_record_is_rate_limited()
 {
    let url = serve().await;
    let config = ChainConfig {
        label: "stand-in node".into(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".into(),
        native_usd: None,
        confirmations: None,
        public_rpc: url,
        logs_rpc: None,
        log_range: None,
    };
    let client =
        SolanaClient::connect_with(&ChainKey::new(ChainKind::Solana, "localnet"), &config, None)
            .await
            .unwrap();
    let payer = Arc::new(Keypair::new());
    let to = Keypair::new().pubkey();
    let error = client
        .send(vec![ix_fund(&payer.pubkey(), &to, 1_000)], &[&payer])
        .await
        .expect_err("the record is never given");
    // The transaction landed: the error says it may have landed, with its
    // signature, and doesn't read as the node's rate limit.
    let late = error
        .downcast_ref::<MayStillLand>()
        .unwrap_or_else(|| panic!("not said to have maybe landed: {error:#}"));
    assert!(!late.signature.is_empty());
    assert!(!rate_limited(&error), "{error:#}");
}
