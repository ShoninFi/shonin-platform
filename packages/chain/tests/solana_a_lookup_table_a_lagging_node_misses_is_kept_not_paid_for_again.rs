//! A lookup table a node behind the cluster answers `null` for is kept, and
//! the send goes through it once it is read: no new table is paid for.
//!
//! `SolanaClient::table_for` reads a key's known tables again before every
//! use. A node behind the cluster (a load balancer's lagging backend, a
//! table made a few slots ago) answers `null` for an account that is there,
//! so a read that finds nothing is not taken as the table being gone. Were
//! it, the key would make and pay for a fresh table, whose rent is never
//! recovered, and for a vault's executor key that rent is billed to the
//! author as gas.
//!
//! A table no read finds is looked for again among the key's tables, and
//! used if found; if the node won't search either, the send fails rather
//! than pay for another table while the first may still be there.
//!
//! The scripted node answers `null` for the keeper's one table as many
//! times as a test says, searches as many times, and refuses any
//! transaction that would make or grow a table. No cluster is needed: the
//! node is a few scripted answers over TCP.

#![cfg(feature = "solana")]

use std::sync::{Arc, Mutex};

use base64::Engine;
use chain::solana::keys::{DEVELOPMENT_MNEMONIC, RoleKeys};
use chain::solana::tables;
use chain::solana::{AccountMeta, Instruction, Pubkey, SolanaClient};
use chain::{ChainConfig, ChainKey, ChainKind, Role};
use serde_json::{Value, json};
use solana_address_lookup_table_interface::state::{AddressLookupTable, LookupTableMeta};
use solana_transaction::versioned::VersionedTransaction;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A genesis that is no public cluster's, so a `localnet` key accepts it.
const GENESIS: &str = "AbfHL7AeBtDMe7HtEA3cXJgzY8cdpa4MTW7ZXk9byEJf";
const SLOT: u64 = 100;

/// What the scripted node has seen and holds.
struct Node {
    table: Pubkey,
    table_data: Vec<u8>,
    /// Reads of the table through `getMultipleAccounts`, and how many of the
    /// first answer `null`, as a node behind the cluster does.
    table_reads: usize,
    misses: usize,
    /// Searches for the key's tables, and how many of the first are
    /// answered: some nodes won't search.
    searches: usize,
    searches_answered: usize,
    /// Transactions the node took, by signature, with their static keys.
    landed: Vec<(String, Vec<Pubkey>)>,
    /// Transactions that would have made or grown a lookup table, refused.
    table_setups: usize,
}

fn account_json(data: &[u8]) -> Value {
    json!({
        "lamports": 10_000_000,
        "owner": tables::PROGRAM.to_string(),
        "executable": false,
        "rentEpoch": 0,
        "data": [base64::engine::general_purpose::STANDARD.encode(data), "base64"],
    })
}

/// One JSON-RPC answer of the scripted node.
fn answer(node: &Mutex<Node>, method: &str, request: &Value) -> Value {
    let params = &request["params"];
    let mut node = node.lock().unwrap();
    let result = match method {
        "getGenesisHash" => json!(GENESIS),
        "getSlot" | "getBlockHeight" => json!(SLOT),
        "getProgramAccounts" => {
            node.searches += 1;
            if node.searches > node.searches_answered {
                return json!({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "error": {"code": -32010, "message": "this node won't search"},
                });
            }
            json!([{
                "pubkey": node.table.to_string(),
                "account": account_json(&node.table_data),
            }])
        }
        "getMultipleAccounts" => {
            node.table_reads += 1;
            let asked: Vec<String> = serde_json::from_value(params[0].clone()).unwrap();
            let value: Vec<Value> = asked
                .iter()
                .map(|address| {
                    if *address == node.table.to_string() && node.table_reads > node.misses {
                        account_json(&node.table_data)
                    } else {
                        Value::Null
                    }
                })
                .collect();
            json!({"context": {"slot": SLOT}, "value": value})
        }
        "getAccountInfo" => {
            let asked = params[0].as_str().unwrap_or_default();
            let value = if asked == node.table.to_string() {
                account_json(&node.table_data)
            } else {
                Value::Null
            };
            json!({"context": {"slot": SLOT}, "value": value})
        }
        "getLatestBlockhash" => json!({
            "context": {"slot": SLOT},
            "value": {
                "blockhash": Pubkey::new_from_array([7; 32]).to_string(),
                "lastValidBlockHeight": SLOT + 150,
            },
        }),
        "sendTransaction" => {
            let wire = base64::engine::general_purpose::STANDARD
                .decode(params[0].as_str().unwrap_or_default())
                .unwrap();
            let tx: VersionedTransaction = bincode::deserialize(&wire).unwrap();
            let keys = tx.message.static_account_keys().to_vec();
            let makes_a_table = tx
                .message
                .instructions()
                .iter()
                .any(|ix| keys.get(ix.program_id_index as usize) == Some(&tables::PROGRAM));
            if makes_a_table {
                node.table_setups += 1;
                return json!({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "error": {
                        "code": -32005,
                        "message": "this node takes no lookup-table set-up: the key has a table",
                        "data": {"err": null, "logs": []},
                    },
                });
            }
            let signature = tx.signatures[0].to_string();
            node.landed.push((signature.clone(), keys));
            json!(signature)
        }
        "getSignatureStatuses" => {
            let asked: Vec<String> = serde_json::from_value(params[0].clone()).unwrap();
            let value: Vec<Value> = asked
                .iter()
                .map(|signature| {
                    if node.landed.iter().any(|(landed, _)| landed == signature) {
                        json!({"slot": SLOT + 1, "err": null, "confirmationStatus": "confirmed"})
                    } else {
                        Value::Null
                    }
                })
                .collect();
            json!({"context": {"slot": SLOT + 1}, "value": value})
        }
        "getTransaction" => {
            let asked = params[0].as_str().unwrap_or_default();
            match node.landed.iter().find(|(landed, _)| landed == asked) {
                Some((_, keys)) => {
                    let keys: Vec<String> = keys.iter().map(ToString::to_string).collect();
                    json!({
                        "slot": SLOT + 1,
                        "blockTime": 1_700_000_000,
                        "version": 0,
                        "transaction": {
                            "message": {"accountKeys": keys, "instructions": []},
                            "signatures": [asked],
                        },
                        "meta": {
                            "err": null,
                            "fee": 5_000,
                            "preBalances": keys.iter().map(|_| 1_000_000_000u64).collect::<Vec<_>>(),
                            "postBalances": keys.iter().enumerate().map(|(at, _)| if at == 0 { 999_995_000u64 } else { 1_000_000_000 }).collect::<Vec<_>>(),
                            "loadedAddresses": {"writable": [], "readonly": []},
                            "logMessages": [],
                            "innerInstructions": [],
                            "computeUnitsConsumed": 1,
                        },
                    })
                }
                None => Value::Null,
            }
        }
        _ => {
            return json!({
                "jsonrpc": "2.0",
                "id": request["id"],
                "error": {"code": -32601, "message": format!("no such method: {method}")},
            });
        }
    };
    json!({"jsonrpc": "2.0", "id": request["id"], "result": result})
}

/// Serve the scripted node on a port of its own, and say where.
async fn serve(node: Arc<Mutex<Node>>) -> String {
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
                    let method = body["method"].as_str().unwrap_or_default().to_string();
                    let text = answer(&node, &method, &body).to_string();
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

/// The keeper's send of a call naming more accounts than the wire carries
/// by address, to a node holding the keeper's one table, which holds them
/// all, whose first `misses` reads of the table answer `null` and whose
/// first `searches_answered` searches for the keeper's tables are answered.
async fn send_through_the_keeper_s_table(
    misses: usize,
    searches_answered: usize,
) -> (anyhow::Result<chain::solana::Landed>, Arc<Mutex<Node>>) {
    let keys = Arc::new(RoleKeys::from_mnemonic(DEVELOPMENT_MNEMONIC).unwrap());
    let keeper = keys.address(Role::Keeper);

    // A call naming more accounts than the wire carries by address.
    let program = Pubkey::new_from_array([3; 32]);
    let named: Vec<Pubkey> = (1u8..=45)
        .map(|n| Pubkey::new_from_array([n; 32]))
        .collect();
    let mut accounts = vec![AccountMeta::new(keeper, true)];
    accounts.extend(named.iter().enumerate().map(|(at, key)| match at {
        0 => AccountMeta::new(*key, false),
        _ => AccountMeta::new_readonly(*key, false),
    }));
    let ix = Instruction {
        program_id: program,
        accounts,
        data: vec![7; 16],
    };
    assert!(
        !tables::fits(&tables::by_address(
            std::slice::from_ref(&ix),
            &keeper,
            Default::default()
        )),
        "fits by address: the test needs a call that doesn't"
    );

    // The keeper's one table on the cluster, holding everything the call
    // names, grown long ago.
    let table = Pubkey::new_from_array([0xaa; 32]);
    let table_data = AddressLookupTable {
        meta: LookupTableMeta {
            last_extended_slot: SLOT - 50,
            last_extended_slot_start_index: 0,
            authority: Some(keeper),
            ..LookupTableMeta::default()
        },
        addresses: std::borrow::Cow::Owned(named.clone()),
    }
    .serialize_for_tests()
    .unwrap();
    let node = Arc::new(Mutex::new(Node {
        table,
        table_data,
        table_reads: 0,
        misses,
        searches: 0,
        searches_answered,
        landed: Vec::new(),
        table_setups: 0,
    }));
    let url = serve(node.clone()).await;
    let config = ChainConfig {
        label: "scripted".into(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".into(),
        native_usd: None,
        confirmations: None,
        public_rpc: url,
        logs_rpc: None,
        log_range: None,
    };
    let client = SolanaClient::connect_with(
        &ChainKey::new(ChainKind::Solana, "localnet"),
        &config,
        Some(keys.clone()),
    )
    .await
    .unwrap();

    let sent = client.send(vec![ix], &[keys.get(Role::Keeper)]).await;
    (sent, node)
}

#[tokio::test]
async fn a_table_a_lagging_read_misses_once_is_used_once_read_and_no_new_table_is_made() {
    // The first read of the table answers `null` (a node behind the
    // cluster); every later read finds it. The send should go through the
    // table, and make none.
    let (sent, node) = send_through_the_keeper_s_table(1, usize::MAX).await;
    let seen = node.lock().unwrap();
    assert!(
        seen.table_reads >= 1,
        "the table was never read before the send"
    );
    assert_eq!(
        seen.table_setups, 0,
        "the keeper's table was dropped after one read that found nothing, and a new one was paid for"
    );
    let landed = match sent {
        Ok(landed) => landed,
        Err(error) => panic!("the send did not go through the keeper's table: {error:#}"),
    };
    assert!(
        landed.setup.is_empty(),
        "the send set a table up, where the keeper's one table held every account"
    );
    assert_eq!(
        seen.landed.len(),
        1,
        "one transaction, through the table, and no other"
    );
}

#[tokio::test]
async fn a_table_no_read_finds_is_found_by_searching_again_and_no_new_table_is_made() {
    let (sent, node) = send_through_the_keeper_s_table(usize::MAX, usize::MAX).await;
    let seen = node.lock().unwrap();
    assert_eq!(seen.table_setups, 0, "a new table was paid for");
    assert!(
        seen.searches >= 2,
        "the key's tables were not searched for again"
    );
    if let Err(error) = sent {
        panic!("the send did not go through the table the search found: {error:#}");
    }
    assert_eq!(seen.landed.len(), 1, "one transaction, through the table");
}

#[tokio::test]
async fn a_table_neither_read_nor_searched_for_fails_the_send_rather_than_pay_for_another() {
    // Found once, when the key's tables are first looked for; then the node
    // neither reads it nor searches again.
    let (sent, node) = send_through_the_keeper_s_table(usize::MAX, 1).await;
    let seen = node.lock().unwrap();
    assert!(sent.is_err(), "the send went through without the table");
    assert_eq!(seen.table_setups, 0, "a new table was paid for");
    assert!(seen.landed.is_empty(), "a transaction went out");
    // Bounded: a few reads, not one for every poll until a timeout.
    assert!(seen.table_reads <= 10, "{} reads", seen.table_reads);
}
