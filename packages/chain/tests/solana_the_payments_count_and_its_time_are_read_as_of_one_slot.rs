//! The count of payments the build service reads and the time it says they
//! were read to come from one answer, as of one slot: the time is never later
//! than what the count holds, so an upload waiting for a payment is never
//! forgotten while a payment made before that time is still uncounted.
//!
//! A stand-in endpoint answers single reads from nodes that disagree (a
//! clock ahead, a count behind) and the one read of both accounts together
//! as of one slot. The count and the time must be the one read's.

#![cfg(feature = "solana")]

use base64::Engine;
use chain::solana::Pubkey;
use chain::solana::SolanaClient;
use chain::solana::platform::{Platform, publishing_config};
use chain::solana::programs::anchor_lang::{AccountSerialize, Space};
use chain::solana::programs::publishing::state::Config;
use chain::{ChainConfig, ChainKey, ChainKind, SolanaDeployments};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CLOCK: &str = "SysvarC1ock11111111111111111111111111111111";

fn config(count: u64) -> Vec<u8> {
    let config = Config {
        mint: Pubkey::default(),
        token_program: Pubkey::default(),
        decimals: 6,
        platform: Pubkey::default(),
        fee: 1,
        count,
        bump: 0,
    };
    let mut out = Vec::with_capacity(8 + Config::INIT_SPACE);
    config.try_serialize(&mut out).unwrap();
    out
}

fn clock(unix: i64) -> Vec<u8> {
    let mut data = vec![0u8; 40];
    data[32..40].copy_from_slice(&unix.to_le_bytes());
    data
}

fn raw(owner: &str, data: &[u8]) -> Value {
    json!({
        "lamports": 1_000_000u64, "owner": owner, "executable": false,
        "data": [base64::engine::general_purpose::STANDARD.encode(data), "base64"],
        "rentEpoch": 0,
    })
}

fn answer(method: &str, params: &Value) -> Value {
    let publishing = chain::solana::programs::publishing::ID.to_string();
    let sysvar = "Sysvar1111111111111111111111111111111111111";
    let config_address = publishing_config().to_string();
    let one = |address: &str| -> Value {
        if address == config_address {
            raw(&publishing, &config(5))
        } else if address == CLOCK {
            raw(sysvar, &clock(1_000))
        } else {
            Value::Null
        }
    };
    match method {
        "getGenesisHash" => json!("stand-in-genesis"),
        // Read alone, each from a node that disagrees with the slot the
        // two are read at together: the count behind, the clock ahead.
        "getAccountInfo" => {
            let address = params[0].as_str().unwrap();
            let value = if address == config_address {
                raw(&publishing, &config(4))
            } else if address == CLOCK {
                raw(sysvar, &clock(2_000))
            } else {
                Value::Null
            };
            json!({"context": {"slot": 9}, "value": value})
        }
        "getMultipleAccounts" => {
            assert_eq!(params[1]["commitment"], "finalized");
            let values: Vec<Value> = params[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|address| one(address.as_str().unwrap()))
                .collect();
            json!({"context": {"slot": 10}, "value": values})
        }
        other => panic!("the stand-in endpoint was asked {other}"),
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
                let result = answer(request["method"].as_str().unwrap(), &request["params"]);
                let reply =
                    json!({"jsonrpc": "2.0", "id": request["id"], "result": result}).to_string();
                let http = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = socket.write_all(http.as_bytes()).await;
            });
        }
    });
    url
}

#[tokio::test]
async fn the_payments_count_and_its_time_are_read_as_of_one_slot() {
    let url = serve().await;
    let config = ChainConfig {
        label: "stand-in endpoint".into(),
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
    let anyone = Pubkey::default().to_string();
    let deployments = SolanaDeployments::from_json(
        &json!({
            "cluster": "devnet",
            "programs": {},
            "mint": anyone,
            "roles": {
                "deployer": anyone, "admin": anyone, "platform": anyone, "attestor": anyone,
                "builder": anyone, "reviewer": anyone, "executor": anyone, "keeper": anyone,
                "operator": anyone, "author": anyone, "viewer": anyone,
            },
        })
        .to_string(),
    )
    .unwrap();
    let platform = Platform::from_deployments(&deployments).unwrap();
    assert_eq!(
        platform.finalized_count(&client).await.unwrap(),
        (5, 1_000),
        "the count and the time are the one read's"
    );
}
