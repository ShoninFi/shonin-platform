//! A stand-in node for the client's tests: it answers the JSON-RPC calls a
//! key client makes to send one transaction, over a bare TCP socket, so the
//! tests need nothing added to the crate's dependencies and no chain. It can
//! misbehave the ways a real node or gateway does: hang up, take its time to
//! answer a send it has already taken, or refuse with a page that names the
//! path it was asked for.
//!
//! For anything else, [`answering`] serves a node whose every answer a test
//! writes itself: a JSON-RPC result, a raw HTTP reply, or nothing at all.
//!
//! Each test file uses part of this, and the rest would be dead code to it.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, B256};
use chain::ChainConfig;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// What the stand-in node has seen and does.
#[derive(Default)]
pub struct Node {
    /// The id it answers `eth_chainId` with.
    pub chain_id: u64,
    /// Transactions sent, in order.
    pub sent: Vec<B256>,
    /// Whether `eth_getTransactionReceipt` has a receipt to give.
    pub mined: bool,
    /// Whether it drops the connection instead of answering anything but
    /// `eth_chainId`.
    pub hangs_up: bool,
    /// How long to hold the reply to `eth_sendRawTransaction` after the
    /// transaction has been recorded.
    pub send_delay: Option<Duration>,
    /// Whether to answer everything but `eth_chainId` with a 404 that echoes
    /// the request path.
    pub echo_path: bool,
}

pub fn node(chain_id: u64) -> Arc<Mutex<Node>> {
    Arc::new(Mutex::new(Node {
        chain_id,
        ..Node::default()
    }))
}

/// What the node says to one request.
enum Reply {
    Json(Value),
    NotFound(String),
    HangUp,
}

/// The answer to one request, and how long to hold it.
fn answer(node: &Mutex<Node>, path: &str, request: &Value) -> (Reply, Option<Duration>) {
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or_default();
    let mut node = node.lock().unwrap();
    if node.hangs_up && method != "eth_chainId" {
        return (Reply::HangUp, None);
    }
    if node.echo_path && method != "eth_chainId" {
        return (Reply::NotFound(format!("Cannot POST {path}")), None);
    }
    let mut hold = None;
    let result = match method {
        "eth_chainId" => json!(format!("0x{:x}", node.chain_id)),
        "eth_accounts" => json!([]),
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
        "eth_estimateGas" => json!("0x5208"),
        "eth_getTransactionCount" => json!(format!("0x{:x}", node.sent.len())),
        "eth_sendRawTransaction" => {
            let raw = request["params"][0].as_str().unwrap();
            let hash = alloy::primitives::keccak256(alloy::hex::decode(raw).unwrap());
            node.sent.push(hash);
            hold = node.send_delay;
            json!(hash)
        }
        "eth_getTransactionReceipt" => {
            if node.mined {
                json!({
                    "transactionHash": node.sent[0],
                    "transactionIndex": "0x0",
                    "blockHash": B256::repeat_byte(0xbb),
                    "blockNumber": "0x11",
                    "from": Address::ZERO,
                    "to": Address::repeat_byte(0x22),
                    "cumulativeGasUsed": "0x5208",
                    "gasUsed": "0x5208",
                    "effectiveGasPrice": "0x3b9aca00",
                    "contractAddress": null,
                    "logs": [],
                    "logsBloom": format!("0x{}", "00".repeat(256)),
                    "status": "0x1",
                    "type": "0x2",
                })
            } else {
                Value::Null
            }
        }
        other => panic!("the stand-in node was asked {other}"),
    };
    (
        Reply::Json(json!({"jsonrpc": "2.0", "id": id, "result": result})),
        hold,
    )
}

/// Serve JSON-RPC over HTTP/1.1 on a port of its own, and say where.
pub async fn start(node: Arc<Mutex<Node>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let node = node.clone();
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                loop {
                    // Read one request: headers, then the body they announce.
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
                    let path = headers
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("/")
                        .to_string();
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

                    let mut hold = None;
                    let reply = match &body {
                        Value::Array(batch) => {
                            let mut answers = Vec::new();
                            let mut refused = None;
                            for one in batch {
                                match answer(&node, &path, one) {
                                    (Reply::Json(value), wait) => {
                                        answers.push(value);
                                        hold = hold.or(wait);
                                    }
                                    (other, _) => refused = Some(other),
                                }
                            }
                            refused.unwrap_or(Reply::Json(Value::Array(answers)))
                        }
                        one => {
                            let (reply, wait) = answer(&node, &path, one);
                            hold = wait;
                            reply
                        }
                    };
                    // The transaction is in, and the node takes its time to
                    // say so; nothing is held while it does.
                    if let Some(wait) = hold {
                        tokio::time::sleep(wait).await;
                    }
                    let response = match reply {
                        Reply::HangUp => return,
                        Reply::Json(value) => {
                            let text = value.to_string();
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{text}",
                                text.len()
                            )
                        }
                        Reply::NotFound(text) => format!(
                            "HTTP/1.1 404 Not Found\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{text}",
                            text.len()
                        ),
                    };
                    if socket.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    format!("http://{address}")
}

pub fn config(url: &str) -> ChainConfig {
    ChainConfig {
        label: "stand-in".to_string(),
        rpc: url.to_string(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: Some(2_500.0),
        confirmations: None,
        public_rpc: url.to_string(),
    }
}

/// What a programmable node says to one request.
pub enum Said {
    /// `{"jsonrpc":"2.0","id":..,"result":..}`
    Result(Value),
    /// This status and body, as they are.
    Raw(u16, String),
    /// Nothing: the connection is held open and never answered.
    Silence,
}

/// How a programmable node answers a request, given its method and the
/// whole request.
pub type Answer = Arc<dyn Fn(&str, &Value) -> Said + Send + Sync>;

/// A refusal of a method the node doesn't know, as a node would give it.
pub fn unknown_method() -> Said {
    Said::Raw(
        200,
        r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32601,"message":"no such method"}}"#
            .to_string(),
    )
}

/// Serve a node that answers every request with `answer`, on a port of its
/// own, and say where. It answers `eth_chainId` itself with `chain_id`, or
/// leaves that to `answer` too when there is none.
pub async fn answering(chain_id: Option<u64>, answer: Answer) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let answer = answer.clone();
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
                    let method = body["method"].as_str().unwrap_or_default().to_string();
                    let said = match (method.as_str(), chain_id) {
                        ("eth_chainId", Some(id)) => Said::Result(json!(format!("0x{id:x}"))),
                        _ => answer(&method, &body),
                    };
                    let (status, text) = match said {
                        Said::Result(result) => (
                            200,
                            json!({"jsonrpc": "2.0", "id": body["id"].clone(), "result": result})
                                .to_string(),
                        ),
                        Said::Raw(status, text) => (status, text),
                        Said::Silence => {
                            std::future::pending::<()>().await;
                            return;
                        }
                    };
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{text}",
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
