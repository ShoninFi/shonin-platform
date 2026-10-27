//! A new lookup table is derived from a slot the bank that runs its
//! creation holds among its recent slots, whatever the cluster's finality
//! lag, never from a slot its leader skipped; and a creation refused for
//! its slot is made again, once the cluster has moved on, from a newer one.
//!
//! The lookup-table program takes only a slot in the running bank's
//! `SlotHashes`: one of the 512 before it that holds a block, never its own
//! slot. The node's preflight runs the creation at the confirmed commitment,
//! so the newest confirmed slot is that bank's own and is refused ("not a
//! recent slot"), and on a cluster whose finalized slot trails the
//! confirmed one by no slots at all, so may the newest finalized one be.
//!
//! The scripted node keeps a confirmed tip that moves on a slot at every
//! read of the slot, a finalized slot behind it by lags a test gives (0 to
//! 2, cycled), and slots a test says were skipped, which hold no block and
//! are in no bank's recent slots. It runs a table's creation as its
//! preflight would, at the tip: refused unless its slot is among the tip
//! bank's recent ones, and refused as many times as a test says whatever
//! its slot, as a backend behind the one read would. A table created is
//! then served, holding what its set-up added. No cluster is needed: the
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
/// The node's confirmed slot when a test starts.
const TIP: u64 = 1_000;
/// How many slots before a bank its `SlotHashes` holds.
const SLOT_HASHES: u64 = 512;

/// What the scripted node has seen and holds.
struct Node {
    /// The newest confirmed slot, the bank the preflight runs at: one
    /// further on at each read of the slot.
    tip: u64,
    /// How far the finalized slot trails the tip, at one read of it after
    /// another.
    finality_lags: Vec<u64>,
    /// How many times the finalized slot has been read.
    finalized_reads: usize,
    /// Slots whose leader made no block.
    skipped: std::ops::RangeInclusive<u64>,
    /// Every creation sent, in order: its slot, whether it was taken, and
    /// the tip it was run at.
    creations: Vec<(u64, bool, u64)>,
    /// How many creations to refuse whatever their slot.
    refuse_creations: usize,
    /// The one table made, and what it holds.
    table: Option<(Pubkey, Pubkey, Vec<Pubkey>)>,
    /// Transactions the node took, by signature, with their static keys.
    landed: Vec<(String, Vec<Pubkey>)>,
}

impl Node {
    fn table_data(&self) -> Option<(Pubkey, Vec<u8>)> {
        let (address, authority, addresses) = self.table.as_ref()?;
        let data = AddressLookupTable {
            meta: LookupTableMeta {
                last_extended_slot: TIP - 1,
                last_extended_slot_start_index: 0,
                authority: Some(*authority),
                ..LookupTableMeta::default()
            },
            addresses: std::borrow::Cow::Owned(addresses.clone()),
        }
        .serialize_for_tests()
        .unwrap();
        Some((*address, data))
    }

    fn table_json(&self, asked: &str) -> Value {
        match self.table_data() {
            Some((address, data)) if address.to_string() == asked => json!({
                "lamports": 10_000_000,
                "owner": tables::PROGRAM.to_string(),
                "executable": false,
                "rentEpoch": 0,
                "data": [base64::engine::general_purpose::STANDARD.encode(data), "base64"],
            }),
            _ => Value::Null,
        }
    }
}

impl Node {
    /// Whether the bank at the tip holds `slot` in its `SlotHashes`: a
    /// block before it, of the 512 before it.
    fn recent(&self, slot: u64) -> bool {
        slot < self.tip && slot + SLOT_HASHES >= self.tip && !self.skipped.contains(&slot)
    }
}

fn refused(request: &Value, slot: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": request["id"],
        "error": {
            "code": -32002,
            "message": "Transaction simulation failed: Error processing Instruction 0: invalid instruction data",
            "data": {
                "err": {"InstructionError": [0, "InvalidInstructionData"]},
                "logs": [
                    format!("Program {} invoke [1]", tables::PROGRAM),
                    format!("{slot} is not a recent slot"),
                    format!("Program {} failed: invalid instruction data", tables::PROGRAM),
                ],
            },
        },
    })
}

/// A transaction as the node runs it: a table's creation or growth checked
/// and applied, anything else taken. `Err` is the node's refusal.
fn run(node: &mut Node, tx: &VersionedTransaction, request: &Value) -> Result<(), Value> {
    let keys = tx.message.static_account_keys();
    for ix in tx.message.instructions() {
        if keys.get(ix.program_id_index as usize) != Some(&tables::PROGRAM) {
            continue;
        }
        let account = |at: usize| keys[ix.accounts[at] as usize];
        let tag = u32::from_le_bytes(ix.data[..4].try_into().unwrap());
        match tag {
            // CreateLookupTable { recent_slot, bump_seed }
            0 => {
                let slot = u64::from_le_bytes(ix.data[4..12].try_into().unwrap());
                let take = node.refuse_creations == 0 && node.recent(slot);
                node.refuse_creations = node.refuse_creations.saturating_sub(1);
                let tip = node.tip;
                node.creations.push((slot, take, tip));
                if !take {
                    return Err(refused(request, slot));
                }
                let authority = account(1);
                let (expected, _) = Pubkey::find_program_address(
                    &[authority.as_ref(), &slot.to_le_bytes()],
                    &tables::PROGRAM,
                );
                assert_eq!(
                    account(0),
                    expected,
                    "the table is not at its slot's address"
                );
                node.table = Some((expected, authority, Vec::new()));
            }
            // ExtendLookupTable { new_addresses }
            2 => {
                let count = u64::from_le_bytes(ix.data[4..12].try_into().unwrap()) as usize;
                let added: Vec<Pubkey> = (0..count)
                    .map(|n| {
                        let at = 12 + 32 * n;
                        Pubkey::new_from_array(ix.data[at..at + 32].try_into().unwrap())
                    })
                    .collect();
                let (address, _, held) = node.table.as_mut().expect("a table to extend");
                assert_eq!(account(0), *address, "a table that was never made");
                held.extend(added);
            }
            other => panic!("an unexpected lookup-table instruction {other}"),
        }
    }
    Ok(())
}

/// One JSON-RPC answer of the scripted node.
fn answer(node: &Mutex<Node>, method: &str, request: &Value) -> Value {
    let params = &request["params"];
    let mut node = node.lock().unwrap();
    let result = match method {
        "getGenesisHash" => json!(GENESIS),
        "getSlot" => {
            let commitment = params[0]["commitment"]
                .as_str()
                .unwrap_or("finalized")
                .to_string();
            node.tip += 1;
            match commitment.as_str() {
                "processed" => json!(node.tip + 2),
                "confirmed" => json!(node.tip),
                _ => {
                    let lag = node.finality_lags[node.finalized_reads % node.finality_lags.len()];
                    node.finalized_reads += 1;
                    json!(node.tip - lag)
                }
            }
        }
        "getBlocks" => {
            let start = params[0].as_u64().unwrap();
            let end = params[1].as_u64().unwrap().min(node.tip);
            let blocks: Vec<u64> = (start..=end)
                .filter(|slot| !node.skipped.contains(slot))
                .collect();
            json!(blocks)
        }
        "getBlockHeight" => json!(TIP),
        "getProgramAccounts" => json!([]),
        "getMultipleAccounts" => {
            let asked: Vec<String> = serde_json::from_value(params[0].clone()).unwrap();
            let value: Vec<Value> = asked.iter().map(|a| node.table_json(a)).collect();
            json!({"context": {"slot": TIP}, "value": value})
        }
        "getAccountInfo" => {
            let value = node.table_json(params[0].as_str().unwrap_or_default());
            json!({"context": {"slot": TIP}, "value": value})
        }
        "getLatestBlockhash" => json!({
            "context": {"slot": TIP},
            "value": {
                "blockhash": Pubkey::new_from_array([7; 32]).to_string(),
                "lastValidBlockHeight": TIP + 150,
            },
        }),
        "sendTransaction" => {
            let wire = base64::engine::general_purpose::STANDARD
                .decode(params[0].as_str().unwrap_or_default())
                .unwrap();
            let tx: VersionedTransaction = bincode::deserialize(&wire).unwrap();
            if let Err(refusal) = run(&mut node, &tx, request) {
                return refusal;
            }
            let signature = tx.signatures[0].to_string();
            node.landed
                .push((signature.clone(), tx.message.static_account_keys().to_vec()));
            json!(signature)
        }
        "getSignatureStatuses" => {
            let asked: Vec<String> = serde_json::from_value(params[0].clone()).unwrap();
            let value: Vec<Value> = asked
                .iter()
                .map(|signature| {
                    if node.landed.iter().any(|(landed, _)| landed == signature) {
                        json!({"slot": TIP - 1, "err": null, "confirmationStatus": "confirmed"})
                    } else {
                        Value::Null
                    }
                })
                .collect();
            json!({"context": {"slot": TIP}, "value": value})
        }
        "getTransaction" => {
            let asked = params[0].as_str().unwrap_or_default();
            match node.landed.iter().find(|(landed, _)| landed == asked) {
                Some((_, keys)) => {
                    let keys: Vec<String> = keys.iter().map(ToString::to_string).collect();
                    json!({
                        "slot": TIP - 1,
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
/// by address, the keeper holding no table yet, to a node whose finalized
/// slot trails its tip by `finality_lags` (cycled), whose `skipped` slots
/// hold no block, and which refuses its first `refuse_creations` creations
/// whatever their slot.
async fn send_needing_a_new_table(
    finality_lags: &[u64],
    skipped: std::ops::RangeInclusive<u64>,
    refuse_creations: usize,
) -> (anyhow::Result<chain::solana::Landed>, Arc<Mutex<Node>>) {
    let keys = Arc::new(RoleKeys::from_mnemonic(DEVELOPMENT_MNEMONIC).unwrap());
    let keeper = keys.address(Role::Keeper);
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

    let node = Arc::new(Mutex::new(Node {
        tip: TIP,
        finality_lags: finality_lags.to_vec(),
        finalized_reads: 0,
        skipped,
        creations: Vec::new(),
        refuse_creations,
        table: None,
        landed: Vec::new(),
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

/// No slot skipped.
#[allow(clippy::reversed_empty_ranges)]
const NONE_SKIPPED: std::ops::RangeInclusive<u64> = 1..=0;

/// The send went through one creation, taken, into a table holding every
/// account the call names.
fn made_once(sent: anyhow::Result<chain::solana::Landed>, seen: &Node) -> u64 {
    let [(slot, taken, tip)] = seen.creations.as_slice() else {
        panic!("one creation, not {:?}", seen.creations);
    };
    assert!(
        *taken,
        "the creation's slot {slot} is not among the recent slots of the bank at {tip}, which runs it"
    );
    let landed = match sent {
        Ok(landed) => landed,
        Err(error) => panic!("the send did not go through its new table: {error:#}"),
    };
    assert_eq!(
        landed.setup.len(),
        2,
        "made with its first addresses, then grown once"
    );
    let (_, _, held) = seen.table.as_ref().unwrap();
    // Every account the call names but the program, whose address is
    // among them and is named by address.
    assert_eq!(
        held.len(),
        44,
        "the table holds every account the call names"
    );
    *slot
}

#[tokio::test]
async fn a_new_table_is_derived_from_a_slot_the_running_bank_holds_when_finality_trails_not_at_all()
{
    let (sent, node) = send_needing_a_new_table(&[0], NONE_SKIPPED, 0).await;
    made_once(sent, &node.lock().unwrap());
}

#[tokio::test]
async fn a_new_table_is_derived_from_a_slot_the_running_bank_holds_when_finality_trails_a_slot_or_two()
 {
    let (sent, node) = send_needing_a_new_table(&[0, 2, 1], NONE_SKIPPED, 0).await;
    made_once(sent, &node.lock().unwrap());
}

#[tokio::test]
async fn a_new_table_is_never_derived_from_a_slot_its_leader_skipped() {
    // Every slot a few dozen behind the tip skipped, so a slot chosen a
    // fixed distance behind it without asking which hold blocks is one.
    let skipped = TIP - 45..=TIP - 15;
    let (sent, node) = send_needing_a_new_table(&[0, 2, 1], skipped.clone(), 0).await;
    let seen = node.lock().unwrap();
    let slot = made_once(sent, &seen);
    assert!(!skipped.contains(&slot), "slot {slot} was skipped");
}

#[tokio::test]
async fn a_creation_refused_for_its_slot_is_made_again_from_a_newer_slot_once_the_cluster_moves_on()
{
    let (sent, node) = send_needing_a_new_table(&[0, 2, 1], NONE_SKIPPED, 1).await;
    let seen = node.lock().unwrap();
    let [(first, false, refused_at), (second, true, taken_at)] = seen.creations.as_slice() else {
        panic!("one refused creation, then one taken: {:?}", seen.creations);
    };
    assert!(second > first, "the second creation reused slot {first}");
    assert!(
        *taken_at >= refused_at + 4,
        "made again at slot {taken_at}, not a few slots on from the refusal at {refused_at}"
    );
    if let Err(error) = sent {
        panic!("the send did not go through the table made again: {error:#}");
    }
}

#[tokio::test]
async fn a_creation_refused_every_time_fails_the_send_after_its_tries() {
    let (sent, node) = send_needing_a_new_table(&[0, 2, 1], NONE_SKIPPED, usize::MAX).await;
    let seen = node.lock().unwrap();
    assert!(sent.is_err(), "the send went through without a table");
    assert_eq!(seen.creations.len(), tables::CREATION_TRIES);
    assert!(
        seen.creations.windows(2).all(|pair| pair[1].0 > pair[0].0),
        "a creation reused a slot: {:?}",
        seen.creations
    );
    assert_eq!(seen.landed.len(), 0, "a transaction went out");
}
