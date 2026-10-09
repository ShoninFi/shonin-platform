//! The vault scan never trusts a count against a listing older than it: a
//! claim a vault's count includes is in every look that returns that count,
//! however a load-balanced endpoint spreads the scan's reads over nodes at
//! different slots.
//!
//! A stand-in endpoint answers every other call from a node a slot behind,
//! which doesn't have the vault's one claim yet (its vault counts none). It
//! honours `minContextSlot` as a node does, refusing a read pinned to a
//! slot it hasn't reached. Whichever node answers each read, every look
//! must return the claim whenever it returns the vault counting it, and the
//! claim must be found. (A claim may be returned before the count that
//! includes it: nothing is missed that way.)

#![cfg(feature = "solana")]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine;
use chain::solana::programs::anchor_lang::{
    AccountDeserialize, AccountSerialize, Discriminator, Space,
};
use chain::solana::programs::vault::state::{self as vs, Claim, Factory, Vault};
use chain::solana::scan::VaultScan;
use chain::solana::{Pubkey, SolanaClient};
use chain::{ChainConfig, ChainKey, ChainKind};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn zeroed<T: AccountDeserialize + Discriminator>(len: usize) -> T {
    let mut bytes = vec![0u8; 8 + len];
    bytes[..8].copy_from_slice(T::DISCRIMINATOR);
    T::try_deserialize(&mut &bytes[..]).expect("zeroed account")
}

fn encode<T: AccountSerialize>(account: &T) -> Vec<u8> {
    let mut out = Vec::new();
    account.try_serialize(&mut out).unwrap();
    out
}

fn raw(data: &[u8]) -> Value {
    json!({
        "lamports": 1_000_000u64,
        "owner": chain::solana::programs::vault::ID.to_string(),
        "executable": false,
        "data": [base64::engine::general_purpose::STANDARD.encode(data), "base64"],
        "rentEpoch": 0,
    })
}

/// One node's view: its slot, the vault as it holds it, and whether it
/// holds the claim.
struct Node {
    slot: u64,
    vault: Vec<u8>,
    claim: bool,
}

struct Endpoint {
    factory: Vec<u8>,
    vault: Pubkey,
    claim: (Pubkey, Vec<u8>),
    /// The node behind, then the node up to date.
    nodes: [Node; 2],
    calls: AtomicU64,
}

impl Endpoint {
    /// The answer, or the node's error.
    fn answer(&self, method: &str, params: &Value) -> Result<Value, Value> {
        if method == "getGenesisHash" {
            return Ok(json!("stand-in-genesis"));
        }
        let node = &self.nodes[(self.calls.fetch_add(1, Ordering::SeqCst) % 2) as usize];
        let config = params.as_array().and_then(|all| all.last()).cloned();
        let at_least = config
            .as_ref()
            .and_then(|config| config["minContextSlot"].as_u64())
            .unwrap_or(0);
        if at_least > node.slot {
            return Err(json!({
                "code": -32016,
                "message": "Minimum context slot has not been reached",
            }));
        }
        let context = json!({"slot": node.slot});
        match method {
            "getAccountInfo" => {
                let address = params[0].as_str().unwrap();
                let value = if address == vs::factory_address().to_string() {
                    raw(&self.factory)
                } else {
                    Value::Null
                };
                Ok(json!({"context": context, "value": value}))
            }
            "getProgramAccounts" => {
                let filter = params[1]["filters"][0]["memcmp"]["bytes"].as_str().unwrap();
                let listed = if filter == bs58::encode(Vault::DISCRIMINATOR).into_string() {
                    json!([{"pubkey": self.vault.to_string(), "account": raw(&node.vault)}])
                } else if filter == bs58::encode(Claim::DISCRIMINATOR).into_string() && node.claim {
                    json!([{"pubkey": self.claim.0.to_string(), "account": raw(&self.claim.1)}])
                } else {
                    json!([])
                };
                if params[1]["withContext"].as_bool() == Some(true) {
                    Ok(json!({"context": context, "value": listed}))
                } else {
                    Ok(listed)
                }
            }
            "getMultipleAccounts" => {
                let values: Vec<Value> = params[0]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|address| {
                        let address = address.as_str().unwrap();
                        if address == vs::factory_address().to_string() {
                            raw(&self.factory)
                        } else if address == self.vault.to_string() {
                            raw(&node.vault)
                        } else if address == self.claim.0.to_string() && node.claim {
                            raw(&self.claim.1)
                        } else {
                            Value::Null
                        }
                    })
                    .collect();
                Ok(json!({"context": context, "value": values}))
            }
            other => panic!("the stand-in endpoint was asked {other}"),
        }
    }
}

async fn serve(endpoint: Arc<Endpoint>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let endpoint = endpoint.clone();
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
                let reply = match endpoint
                    .answer(request["method"].as_str().unwrap(), &request["params"])
                {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": request["id"], "result": result}),
                    Err(error) => json!({"jsonrpc": "2.0", "id": request["id"], "error": error}),
                }
                .to_string();
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
async fn a_claim_a_vault_counts_is_never_missed_for_a_node_behind() {
    let strategy_id = 7;
    let vault_address = vs::vault_address(strategy_id).0;
    let mut factory: Factory = zeroed(Factory::INIT_SPACE);
    factory.vault_count = 1;
    let mut vault: Vault = zeroed(Vault::INIT_SPACE);
    vault.strategy_id = strategy_id;
    let before = encode(&vault);
    vault.claim_count = 1;
    let after = encode(&vault);
    let mut claim: Claim = zeroed(Claim::INIT_SPACE);
    claim.id = 1;
    let claim_address = vs::claim_address(&vault_address, claim.id).0;
    let endpoint = Arc::new(Endpoint {
        factory: encode(&factory),
        vault: vault_address,
        claim: (claim_address, encode(&claim)),
        nodes: [
            Node {
                slot: 1,
                vault: before,
                claim: false,
            },
            Node {
                slot: 2,
                vault: after,
                claim: true,
            },
        ],
        calls: AtomicU64::new(0),
    });
    let url = serve(endpoint.clone()).await;
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

    // Each start of the scan meets the nodes in another order.
    for skip in 0..4 {
        endpoint.calls.store(skip, Ordering::SeqCst);
        // As on a public cluster: the program listed at most every five
        // minutes, its counts trusted in between.
        let mut scan = VaultScan::new(Duration::from_secs(5 * 60));
        let mut found_it = false;
        for look in 0..6 {
            let found = scan.look(&client).await.unwrap();
            let counted: u64 = found.vaults.iter().map(|(_, v)| v.claim_count).sum();
            assert!(
                found.claims.len() as u64 >= counted,
                "start {skip}, look {look}: the vaults count {counted} claims and the look \
                 returned {}",
                found.claims.len()
            );
            found_it |= found.claims.iter().any(|(_, at, _)| *at == claim_address);
        }
        assert!(found_it, "start {skip}: the claim was never found");
    }
}
