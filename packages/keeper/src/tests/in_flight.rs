//! The keeper's sends from a key it signs for, several in flight at once:
//! against a stand-in node with a pool, which numbers nothing itself, takes
//! signed transactions, and mines them in nonce order a block at a time.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::consensus::transaction::{Recovered, SignerRecoverable};
use alloy::consensus::{Transaction as _, TxEnvelope};
use alloy::eips::eip2718::Decodable2718;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use super::*;

/// A transaction the stand-in has taken.
#[derive(Clone)]
struct Taken {
    envelope: TxEnvelope,
    from: Address,
}

impl Taken {
    fn hash(&self) -> B256 {
        *self.envelope.tx_hash()
    }

    fn nonce(&self) -> u64 {
        self.envelope.nonce()
    }

    fn to(&self) -> Option<Address> {
        self.envelope.to()
    }

    fn fee(&self) -> u128 {
        self.envelope
            .gas_price()
            .unwrap_or_else(|| self.envelope.max_fee_per_gas())
    }
}

/// A transaction mined: in which block, and whether it went through.
#[derive(Clone)]
struct Landed {
    block: u64,
    ok: bool,
    /// What was mined; none for a transaction made outside the keeper.
    taken: Option<Taken>,
    from: Address,
    nonce: u64,
}

/// The stand-in node with a pool.
#[derive(Default)]
struct Pool {
    head: u64,
    /// Each account's count of mined transactions.
    mined: HashMap<Address, u64>,
    /// What waits in the pool, by sender and nonce.
    pool: BTreeMap<(Address, u64), Taken>,
    /// What was mined, by hash.
    landed: HashMap<B256, Landed>,
    /// The hash of every raw transaction handed to it, in order, refused or
    /// not.
    handed: Vec<B256>,
    /// How many of the next transactions it takes and then loses, as a
    /// node does that drops a transaction from its pool.
    loses: usize,
    /// Whether a transaction it lost is refused when sent again.
    refuses_lost: bool,
    lost: HashSet<B256>,
    /// Calls to these are kept in the pool and never mined: underpriced for
    /// the block, say.
    never_mines: HashSet<Address>,
    /// Calls to these are mined and revert.
    reverts: HashSet<Address>,
    /// Before it takes the next transaction, another of the same key's is
    /// mined at that key's next nonce: an external send.
    external_first: bool,
    /// Whether its count of an account's sends, pending ones included, is
    /// one past the highest nonce it holds, gaps or not, as some nodes
    /// count them.
    counts_past_gaps: bool,
    /// Whether its answers about a transaction (its receipt, the
    /// transaction itself) come from a node behind the one that counts the
    /// account's sends, as behind a load balancer: none yet.
    lagging_receipts: bool,
}

type Shared = Arc<Mutex<Pool>>;

const SIGNED_FOR: u64 = 31_337;

impl Pool {
    fn pending(&self, from: Address) -> u64 {
        let mut next = self.mined.get(&from).copied().unwrap_or_default();
        if self.counts_past_gaps {
            return self
                .pool
                .keys()
                .filter(|(sender, _)| *sender == from)
                .map(|(_, nonce)| nonce + 1)
                .max()
                .unwrap_or(next)
                .max(next);
        }
        while self.pool.contains_key(&(from, next)) {
            next += 1;
        }
        next
    }

    /// Mine a transaction made outside the keeper at `from`'s next nonce,
    /// in a block of its own; whatever waited at that nonce is gone.
    fn external(&mut self, from: Address) {
        let nonce = self.mined.get(&from).copied().unwrap_or_default();
        self.pool.remove(&(from, nonce));
        self.head += 1;
        let hash =
            alloy::primitives::keccak256([b"external".as_slice(), &nonce.to_be_bytes()].concat());
        self.landed.insert(
            hash,
            Landed {
                block: self.head,
                ok: true,
                taken: None,
                from,
                nonce,
            },
        );
        self.mined.insert(from, nonce + 1);
    }

    /// Mine a block: every account's transactions waiting at its next
    /// nonces, in order, up to a gap or one that never mines.
    fn mine(&mut self) {
        self.head += 1;
        let senders: Vec<Address> = self.pool.keys().map(|(from, _)| *from).collect();
        for from in senders {
            loop {
                let next = self.mined.get(&from).copied().unwrap_or_default();
                let Some(taken) = self.pool.get(&(from, next)).cloned() else {
                    break;
                };
                if taken.to().is_some_and(|to| self.never_mines.contains(&to)) {
                    break;
                }
                self.pool.remove(&(from, next));
                let ok = !taken.to().is_some_and(|to| self.reverts.contains(&to));
                self.landed.insert(
                    taken.hash(),
                    Landed {
                        block: self.head,
                        ok,
                        taken: Some(taken),
                        from,
                        nonce: next,
                    },
                );
                self.mined.insert(from, next + 1);
            }
        }
    }

    fn take(&mut self, raw: &[u8]) -> Result<B256, String> {
        let envelope =
            TxEnvelope::decode_2718(&mut &raw[..]).map_err(|_| "rlp: undecodable".to_string())?;
        let from = envelope
            .recover_signer()
            .map_err(|_| "invalid sender".to_string())?;
        let taken = Taken { envelope, from };
        let hash = taken.hash();
        self.handed.push(hash);
        if std::mem::take(&mut self.external_first) {
            self.external(from);
        }
        if self.landed.contains_key(&hash) {
            return Err("already known".to_string());
        }
        if taken.nonce() < self.mined.get(&from).copied().unwrap_or_default() {
            return Err("nonce too low".to_string());
        }
        if self.lost.contains(&hash) && self.refuses_lost {
            return Err("transaction underpriced".to_string());
        }
        if let Some(waiting) = self.pool.get(&(from, taken.nonce())) {
            if waiting.hash() == hash {
                return Err("already known".to_string());
            }
            if taken.fee() * 10 < waiting.fee() * 11 {
                return Err("replacement transaction underpriced".to_string());
            }
        }
        if self.loses > 0 && !self.lost.contains(&hash) {
            self.loses -= 1;
            self.lost.insert(hash);
            return Ok(hash);
        }
        self.pool.insert((from, taken.nonce()), taken);
        Ok(hash)
    }

    fn known(&self, hash: B256) -> Option<(Taken, Option<u64>)> {
        if let Some(landed) = self.landed.get(&hash) {
            return landed
                .taken
                .clone()
                .map(|taken| (taken, Some(landed.block)));
        }
        self.pool
            .values()
            .find(|taken| taken.hash() == hash)
            .map(|taken| (taken.clone(), None))
    }

    fn answer(&mut self, asked: &Value) -> Value {
        let id = asked["id"].clone();
        let params = &asked["params"];
        let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
        let refused = |message: String| json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": message}});
        let hash_param = || -> B256 { params[0].as_str().unwrap().parse().unwrap() };
        match asked["method"].as_str().unwrap_or_default() {
            "eth_getTransactionReceipt" | "eth_getTransactionByHash" if self.lagging_receipts => {
                ok(Value::Null)
            }
            "eth_chainId" => ok(json!(format!("0x{SIGNED_FOR:x}"))),
            "eth_blockNumber" => ok(json!(format!("0x{:x}", self.head))),
            "eth_gasPrice" | "eth_maxPriorityFeePerGas" => ok(json!("0x3b9aca00")),
            "eth_feeHistory" => ok(json!({
                "oldestBlock": "0x1",
                "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                "gasUsedRatio": [0.5],
                "reward": [["0x3b9aca00"]]
            })),
            "eth_getBlockByNumber" => {
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.number = self.head;
                block.header.inner.base_fee_per_gas = Some(1_000_000_000);
                ok(serde_json::to_value(block).unwrap())
            }
            "eth_estimateGas" => ok(json!("0x30000")),
            "eth_getTransactionCount" => {
                let from: Address = params[0].as_str().unwrap().parse().unwrap();
                let count = match params[1].as_str() {
                    Some("pending") => self.pending(from),
                    _ => self.mined.get(&from).copied().unwrap_or_default(),
                };
                ok(json!(format!("0x{count:x}")))
            }
            "eth_sendRawTransaction" => {
                let raw = alloy::hex::decode(params[0].as_str().unwrap()).unwrap();
                match self.take(&raw) {
                    Ok(hash) => ok(json!(hash)),
                    Err(message) => refused(message),
                }
            }
            "eth_getTransactionReceipt" => match self.landed.get(&hash_param()) {
                Some(landed) => ok(json!({
                    "transactionHash": hash_param(),
                    "transactionIndex": "0x0",
                    "blockHash": B256::with_last_byte(landed.block as u8),
                    "blockNumber": format!("0x{:x}", landed.block),
                    "from": landed.from,
                    "to": landed.taken.as_ref().and_then(Taken::to),
                    "cumulativeGasUsed": "0x5208",
                    "gasUsed": "0x5208",
                    "effectiveGasPrice": "0x3b9aca00",
                    "contractAddress": null,
                    "logs": [],
                    "logsBloom": format!("0x{}", "00".repeat(256)),
                    "status": if landed.ok { "0x1" } else { "0x0" },
                    "type": "0x2",
                })),
                None => ok(Value::Null),
            },
            "eth_getTransactionByHash" => match self.known(hash_param()) {
                Some((taken, block)) => {
                    let transaction = alloy::rpc::types::Transaction {
                        inner: Recovered::new_unchecked(taken.envelope.clone(), taken.from),
                        block_hash: block.map(|block| B256::with_last_byte(block as u8)),
                        block_number: block,
                        transaction_index: block.map(|_| 0),
                        effective_gas_price: None,
                        block_timestamp: None,
                    };
                    ok(serde_json::to_value(transaction).unwrap())
                }
                None => ok(Value::Null),
            },
            other => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("not scripted: {other}")}})
            }
        }
    }
}

/// The stand-in node with a pool, mining a block every `every` when given
/// one, and only when told to otherwise.
async fn a_node_with_a_pool(every: Option<Duration>) -> (Shared, String) {
    let pool: Shared = Arc::default();
    pool.lock().unwrap().head = 16;
    let app = Router::new()
        .route(
            "/",
            post(
                |State(pool): State<Shared>, Json(body): Json<Value>| async move {
                    let mut pool = pool.lock().unwrap();
                    Json(match body {
                        Value::Array(batch) => {
                            Value::Array(batch.iter().map(|one| pool.answer(one)).collect())
                        }
                        one => pool.answer(&one),
                    })
                },
            ),
        )
        .with_state(pool.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    if let Some(every) = every {
        let pool = pool.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                pool.lock().unwrap().mine();
            }
        });
    }
    (pool, url)
}

/// The keeper's client on the stand-in, signing for itself as on a
/// testnet, its sends in flight together, each waited for at most `wait`.
async fn the_keeper_on(url: &str, wait: Duration) -> (Observed, Address) {
    let keys = chain::keys_from_mnemonic(
        "test test test test test test test test test test test junk",
        1,
    )
    .unwrap();
    let keeper = keys[0].address();
    let config = chain::ChainConfig {
        label: "stand-in with a pool".to_string(),
        rpc: url.to_string(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        confirmations: None,
        public_rpc: url.to_string(),
        logs_rpc: None,
        log_range: None,
    };
    let client = Client::connect(&config, chain::Signing::Keys(keys))
        .await
        .unwrap()
        .pipelined()
        .waiting_at_most(wait);
    receipts_polled_every(&client, Duration::from_millis(50));
    (client.into(), keeper)
}

fn vault(n: u8) -> Address {
    Address::repeat_byte(0x40 + n)
}

/// Value `vault` as the keeper does, and say what became of it.
async fn value(client: &Observed, keeper: Address, vault: Address) -> Outcome {
    send_outcome(
        client,
        keeper,
        vault,
        IVault::recordValuationCall {},
        "value",
    )
    .await
}

/// Wait until the pool holds `count` transactions.
async fn until_pooled(pool: &Shared, count: usize) {
    for _ in 0..400 {
        if pool.lock().unwrap().pool.len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "the pool never held {count} transactions: it holds {}",
        pool.lock().unwrap().pool.len()
    );
}

/// The keeper's transactions mined, as (call's target, nonce, block, went
/// through), in nonce order.
fn mined(pool: &Shared, keeper: Address) -> Vec<(Option<Address>, u64, u64, bool)> {
    let pool = pool.lock().unwrap();
    let mut mined: Vec<_> = pool
        .landed
        .values()
        .filter(|landed| landed.from == keeper)
        .map(|landed| {
            (
                landed.taken.as_ref().and_then(Taken::to),
                landed.nonce,
                landed.block,
                landed.ok,
            )
        })
        .collect();
    mined.sort_by_key(|(_, nonce, ..)| *nonce);
    mined
}

/// Three vaults valued at once: every valuation is handed to the node
/// before any is mined, and all three land in one block, one nonce each.
#[tokio::test]
async fn several_sends_in_flight_land_in_one_block() {
    let (pool, url) = a_node_with_a_pool(None).await;
    let (client, keeper) = the_keeper_on(&url, Duration::from_secs(20)).await;
    let sends = futures::future::join_all((1..=3).map(|n| value(&client, keeper, vault(n))));
    let miner = async {
        until_pooled(&pool, 3).await;
        pool.lock().unwrap().mine();
    };
    let (outcomes, ()) = tokio::join!(sends, miner);
    assert_eq!(outcomes, vec![Outcome::Done; 3]);
    let mined = mined(&pool, keeper);
    assert_eq!(mined.len(), 3, "{mined:?}");
    let block = mined[0].2;
    assert!(
        mined.iter().all(|(_, _, at, ok)| *at == block && *ok),
        "the three valuations did not land together: {mined:?}"
    );
    let nonces: Vec<u64> = mined.iter().map(|(_, nonce, ..)| *nonce).collect();
    assert_eq!(nonces, vec![0, 1, 2]);
}

/// A revert in the middle of three sends in flight is seen as the vault's
/// refusal on the chain, and the sends on either side of it land.
#[tokio::test]
async fn a_revert_between_sends_in_flight_is_seen_and_the_others_land() {
    let (pool, url) = a_node_with_a_pool(None).await;
    pool.lock().unwrap().reverts.insert(vault(2));
    let (client, keeper) = the_keeper_on(&url, Duration::from_secs(20)).await;
    let sends = async {
        let first = value(&client, keeper, vault(1));
        let second = async {
            until_pooled(&pool, 1).await;
            value(&client, keeper, vault(2)).await
        };
        let third = async {
            until_pooled(&pool, 2).await;
            value(&client, keeper, vault(3)).await
        };
        tokio::join!(first, second, third)
    };
    let miner = async {
        until_pooled(&pool, 3).await;
        pool.lock().unwrap().mine();
    };
    let ((first, second, third), ()) = tokio::join!(sends, miner);
    assert_eq!(
        (first, second, third),
        (Outcome::Done, Outcome::Reverted, Outcome::Done)
    );
    let mined = mined(&pool, keeper);
    assert_eq!(
        mined
            .iter()
            .map(|(to, nonce, _, ok)| (*to, *nonce, *ok))
            .collect::<Vec<_>>(),
        vec![
            (Some(vault(1)), 0, true),
            (Some(vault(2)), 1, false),
            (Some(vault(3)), 2, true)
        ]
    );
}

/// A send the node took and then lost, with another in flight behind it:
/// it is handed over again as the same bytes, so it lands once, and the
/// send behind it lands after it.
#[tokio::test]
async fn a_dropped_send_is_handed_over_again_as_the_same_transaction() {
    let (pool, url) = a_node_with_a_pool(Some(Duration::from_millis(100))).await;
    pool.lock().unwrap().loses = 1;
    let (client, keeper) = the_keeper_on(&url, Duration::from_secs(20)).await;
    let (first, second) = tokio::join!(value(&client, keeper, vault(1)), async {
        // After the first was taken (and lost).
        for _ in 0..400 {
            if !pool.lock().unwrap().handed.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        value(&client, keeper, vault(2)).await
    });
    assert_eq!((first, second), (Outcome::Done, Outcome::Done));
    let pool_now = pool.lock().unwrap();
    let lost = *pool_now.lost.iter().next().unwrap();
    assert!(
        pool_now.landed.contains_key(&lost),
        "the lost transaction itself did not land"
    );
    assert!(
        pool_now.handed.iter().filter(|hash| **hash == lost).count() >= 2,
        "the lost transaction was not handed over again"
    );
    drop(pool_now);
    let mined = mined(&pool, keeper);
    assert_eq!(
        mined
            .iter()
            .map(|(to, nonce, ..)| (*to, *nonce))
            .collect::<Vec<_>>(),
        vec![(Some(vault(1)), 0), (Some(vault(2)), 1)]
    );
}

/// A send the node lost and then refuses (its fee too low by then): its
/// nonce is filled with a transfer of nothing, so the send behind it still
/// lands; the lost send is said not to have gone out, and never lands.
#[tokio::test]
async fn a_lost_send_the_node_refuses_again_is_filled_and_holds_no_later_send() {
    let (pool, url) = a_node_with_a_pool(Some(Duration::from_millis(100))).await;
    {
        let mut pool = pool.lock().unwrap();
        pool.loses = 1;
        pool.refuses_lost = true;
    }
    let (client, keeper) = the_keeper_on(&url, Duration::from_secs(20)).await;
    let (first, second) = tokio::join!(value(&client, keeper, vault(1)), async {
        for _ in 0..400 {
            if !pool.lock().unwrap().handed.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        value(&client, keeper, vault(2)).await
    });
    assert_eq!((first, second), (Outcome::NotSent, Outcome::Done));
    let mined = mined(&pool, keeper);
    assert_eq!(
        mined
            .iter()
            .map(|(to, nonce, _, ok)| (*to, *nonce, *ok))
            .collect::<Vec<_>>(),
        vec![(Some(keeper), 0, true), (Some(vault(2)), 1, true)],
        "the lost send's nonce was not filled, or the lost send landed"
    );
}

/// A send that waits in the pool and never mines: two thirds of the way to
/// its deadline its nonce is filled at a higher fee, the send behind it
/// lands, and the stuck send is said not to have gone out (it never can).
#[tokio::test]
async fn a_stuck_send_is_filled_before_its_deadline_and_holds_no_later_send() {
    let (pool, url) = a_node_with_a_pool(Some(Duration::from_millis(100))).await;
    pool.lock().unwrap().never_mines.insert(vault(1));
    let wait = Duration::from_secs(3);
    let (client, keeper) = the_keeper_on(&url, wait).await;
    let started = std::time::Instant::now();
    let (first, second) = tokio::join!(value(&client, keeper, vault(1)), async {
        until_pooled(&pool, 1).await;
        value(&client, keeper, vault(2)).await
    });
    assert!(
        started.elapsed() < wait + Duration::from_secs(1),
        "the sends waited past their deadline"
    );
    assert_eq!((first, second), (Outcome::NotSent, Outcome::Done));
    let mined = mined(&pool, keeper);
    assert_eq!(
        mined
            .iter()
            .map(|(to, nonce, _, ok)| (*to, *nonce, *ok))
            .collect::<Vec<_>>(),
        vec![(Some(keeper), 0, true), (Some(vault(2)), 1, true)]
    );
}

/// The keeper's valuations of `vaults`, one after another is handed over,
/// made by a process (a runtime of its own) that stops once they are all
/// handed over and before any lands: nothing of it watches or fills them
/// after.
async fn a_process_that_stops(url: &str, pool: &Shared, vaults: Vec<Address>) {
    let (url, pool) = (url.to_string(), pool.clone());
    tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (client, keeper) = the_keeper_on(&url, Duration::from_secs(20)).await;
            for (sent, vault) in vaults.into_iter().enumerate() {
                let client = client.clone();
                tokio::spawn(async move { value(&client, keeper, vault).await });
                for _ in 0..400 {
                    if pool.lock().unwrap().handed.len() > sent {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        });
    })
    .await
    .unwrap();
}

/// A keeper started again while its last process's sends wait in the
/// pool numbers its own past them; one whose last process left a gap (a
/// send lost below one still waiting) fills it with its first send.
#[tokio::test]
async fn a_keeper_started_again_numbers_its_sends_past_what_its_last_process_left() {
    let keeper = chain::keys_from_mnemonic(
        "test test test test test test test test test test test junk",
        1,
    )
    .unwrap()[0]
        .address();
    // The last process's sends at nonces 5 and 6, waiting.
    let (pool, url) = a_node_with_a_pool(None).await;
    pool.lock().unwrap().mined.insert(keeper, 5);
    a_process_that_stops(&url, &pool, vec![vault(1), vault(2)]).await;
    until_pooled(&pool, 2).await;
    let (again, _) = the_keeper_on(&url, Duration::from_secs(20)).await;
    let (outcome, ()) = tokio::join!(value(&again, keeper, vault(3)), async {
        until_pooled(&pool, 3).await;
        pool.lock().unwrap().mine();
    });
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(
        mined(&pool, keeper)
            .iter()
            .map(|(to, nonce, ..)| (*to, *nonce))
            .collect::<Vec<_>>(),
        vec![
            (Some(vault(1)), 5),
            (Some(vault(2)), 6),
            (Some(vault(3)), 7)
        ]
    );

    // A last process that left a gap: its send at 7 lost, the one at 8
    // waiting behind it.
    let (pool, url) = a_node_with_a_pool(None).await;
    pool.lock().unwrap().mined.insert(keeper, 7);
    pool.lock().unwrap().loses = 1;
    a_process_that_stops(&url, &pool, vec![vault(1), vault(2)]).await;
    until_pooled(&pool, 1).await;
    let (again, _) = the_keeper_on(&url, Duration::from_secs(20)).await;
    let (outcome, ()) = tokio::join!(value(&again, keeper, vault(3)), async {
        until_pooled(&pool, 2).await;
        pool.lock().unwrap().mine();
    });
    assert_eq!(outcome, Outcome::Done);
    assert_eq!(
        mined(&pool, keeper)
            .iter()
            .map(|(to, nonce, ..)| (*to, *nonce))
            .collect::<Vec<_>>(),
        vec![(Some(vault(3)), 7), (Some(vault(2)), 8)],
        "the gap was not filled by the first send after the restart"
    );
}

/// A node that counts an account's sends past a gap: a keeper started
/// again after its last process lost a send below one still waiting is
/// numbered past both, behind the gap. The gap, which no send of its own
/// holds, is filled with a transfer of nothing, and its send lands.
#[tokio::test]
async fn a_gap_no_send_holds_is_filled_and_the_sends_behind_it_land() {
    let keeper = chain::keys_from_mnemonic(
        "test test test test test test test test test test test junk",
        1,
    )
    .unwrap()[0]
        .address();
    let (pool, url) = a_node_with_a_pool(Some(Duration::from_millis(100))).await;
    {
        let mut pool = pool.lock().unwrap();
        pool.mined.insert(keeper, 7);
        pool.loses = 1;
        pool.counts_past_gaps = true;
    }
    a_process_that_stops(&url, &pool, vec![vault(1), vault(2)]).await;
    let (again, _) = the_keeper_on(&url, Duration::from_secs(3)).await;
    assert_eq!(value(&again, keeper, vault(3)).await, Outcome::Done);
    assert_eq!(
        mined(&pool, keeper)
            .iter()
            .map(|(to, nonce, ..)| (*to, *nonce))
            .collect::<Vec<_>>(),
        vec![(Some(keeper), 7), (Some(vault(2)), 8), (Some(vault(3)), 9)],
        "the gap was not filled"
    );
}

/// Another send of the same key: one that takes the nonce a send was
/// numbered at before it is handed over has the send numbered again past
/// it; one that takes the nonce of a send already in flight has that send
/// said not to have gone out (it can never land), and the vault's
/// valuation sent again lands once.
#[tokio::test]
async fn a_send_of_the_same_key_from_elsewhere_never_has_a_duty_sent_twice() {
    // Taken just before the keeper's is handed over.
    let (pool, url) = a_node_with_a_pool(Some(Duration::from_millis(100))).await;
    pool.lock().unwrap().external_first = true;
    let (client, keeper) = the_keeper_on(&url, Duration::from_secs(20)).await;
    assert_eq!(value(&client, keeper, vault(1)).await, Outcome::Done);
    let mined_now = mined(&pool, keeper);
    assert_eq!(
        mined_now
            .iter()
            .map(|(to, nonce, ..)| (*to, *nonce))
            .collect::<Vec<_>>(),
        vec![(None, 0), (Some(vault(1)), 1)]
    );

    // Taken while the keeper's waits in the pool.
    let (pool, url) = a_node_with_a_pool(None).await;
    pool.lock().unwrap().never_mines.insert(vault(1));
    let (client, keeper) = the_keeper_on(&url, Duration::from_secs(20)).await;
    let (outcome, ()) = tokio::join!(value(&client, keeper, vault(1)), async {
        until_pooled(&pool, 1).await;
        pool.lock().unwrap().external(keeper);
        // Blocks go on being mined.
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            pool.lock().unwrap().mine();
        }
    });
    assert_eq!(outcome, Outcome::NotSent);
    // Sent again, it lands once, after the other.
    pool.lock().unwrap().never_mines.clear();
    let (again, ()) = tokio::join!(value(&client, keeper, vault(1)), async {
        until_pooled(&pool, 1).await;
        pool.lock().unwrap().mine();
    });
    assert_eq!(again, Outcome::Done);
    assert_eq!(
        mined(&pool, keeper)
            .iter()
            .map(|(to, nonce, ..)| (*to, *nonce))
            .collect::<Vec<_>>(),
        vec![(None, 0), (Some(vault(1)), 1)],
        "the valuation landed twice, or not after the other send"
    );
}

/// A valuation that landed is not said not to have gone out because the
/// node answering for its receipt is some blocks behind the one that counts
/// the account's sends: its receipt is looked for until it comes.
#[tokio::test]
async fn a_landed_send_whose_receipt_lags_the_count_is_seen_to_land() {
    let (pool, url) = a_node_with_a_pool(Some(Duration::from_millis(100))).await;
    let (client, keeper) = the_keeper_on(&url, Duration::from_secs(20)).await;
    let lagging = async {
        until_pooled(&pool, 1).await;
        pool.lock().unwrap().lagging_receipts = true;
        // Mined, its receipt ten blocks behind.
        while mined(&pool, keeper).is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        pool.lock().unwrap().lagging_receipts = false;
    };
    let (outcome, ()) = tokio::join!(value(&client, keeper, vault(1)), lagging);
    assert_eq!(
        mined(&pool, keeper),
        vec![(Some(vault(1)), 0, 17, true)],
        "it landed"
    );
    assert_eq!(outcome, Outcome::Done, "a landed valuation said not sent");
}

/// A landed send whose receipt no node answering gives before its deadline
/// is said never to have landed, which may yet be wrong, never superseded:
/// nothing says another transaction took its nonce.
#[tokio::test]
async fn a_landed_send_whose_receipt_never_comes_is_not_said_superseded() {
    let (pool, url) = a_node_with_a_pool(Some(Duration::from_millis(100))).await;
    let (client, keeper) = the_keeper_on(&url, Duration::from_secs(2)).await;
    let lagging = async {
        until_pooled(&pool, 1).await;
        pool.lock().unwrap().lagging_receipts = true;
    };
    let (sent, ()) = tokio::join!(
        client.send(keeper, vault(1), IVault::recordValuationCall {}),
        lagging
    );
    let error = sent.expect_err("no receipt was ever given");
    assert_eq!(mined(&pool, keeper).len(), 1, "it landed");
    assert!(
        !error
            .chain()
            .any(|cause| cause.is::<chain::client::Superseded>()),
        "a landed send was said superseded: {error:#}"
    );
    assert!(chain::unanswered(&error), "{error:#}");
}

/// A send whose nonce another transaction of the same key took is still
/// said to have been superseded, once its receipt has stayed missing for
/// longer than a node behind another explains, and before its deadline.
#[tokio::test]
async fn a_send_whose_nonce_another_took_is_said_superseded_before_its_deadline() {
    let (pool, url) = a_node_with_a_pool(Some(Duration::from_millis(50))).await;
    pool.lock().unwrap().never_mines.insert(vault(1));
    let wait = Duration::from_secs(20);
    let (client, keeper) = the_keeper_on(&url, wait).await;
    let started = std::time::Instant::now();
    let (sent, ()) = tokio::join!(
        client.send(keeper, vault(1), IVault::recordValuationCall {}),
        async {
            until_pooled(&pool, 1).await;
            pool.lock().unwrap().external(keeper);
        }
    );
    let error = sent.expect_err("its nonce was taken");
    assert!(
        error
            .chain()
            .any(|cause| cause.is::<chain::client::Superseded>()),
        "{error:#}"
    );
    assert!(
        started.elapsed() < wait / 2,
        "it was said superseded only at its deadline"
    );
    assert_eq!(
        mined(&pool, keeper)
            .iter()
            .map(|(to, nonce, ..)| (*to, *nonce))
            .collect::<Vec<_>>(),
        vec![(None, 0)]
    );
}

/// A node that signs for the keeper numbers each send as it takes it: a
/// pipelined keeper hands it one send at a time (two at once could be
/// numbered the same), and still has several in flight.
#[tokio::test]
async fn a_node_that_signs_is_handed_one_send_at_a_time() {
    #[derive(Default)]
    struct Taking {
        now: usize,
        most: usize,
        taken: usize,
    }
    let keeper = Address::repeat_byte(0x4b);
    let taking: Arc<Mutex<Taking>> = Arc::default();
    let app = Router::new()
        .route(
            "/",
            post(
                move |State(taking): State<Arc<Mutex<Taking>>>, Json(asked): Json<Value>| async move {
                    let id = asked["id"].clone();
                    let ok = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
                    Json(match asked["method"].as_str().unwrap_or_default() {
                        "eth_chainId" => ok(json!(format!("0x{SIGNED_FOR:x}"))),
                        "eth_accounts" => ok(json!([keeper])),
                        "eth_sendTransaction" => {
                            let number = {
                                let mut taking = taking.lock().unwrap();
                                taking.now += 1;
                                taking.most = taking.most.max(taking.now);
                                taking.taken += 1;
                                taking.taken
                            };
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            taking.lock().unwrap().now -= 1;
                            ok(json!(B256::with_last_byte(number as u8)))
                        }
                        // Mined once all three are taken.
                        "eth_getTransactionReceipt" if taking.lock().unwrap().taken < 3 => {
                            ok(Value::Null)
                        }
                        "eth_getTransactionReceipt" => ok(json!({
                            "transactionHash": asked["params"][0],
                            "transactionIndex": "0x0",
                            "blockHash": B256::repeat_byte(0xbb),
                            "blockNumber": "0x11",
                            "from": keeper,
                            "to": vault(1),
                            "cumulativeGasUsed": "0x5208",
                            "gasUsed": "0x5208",
                            "effectiveGasPrice": "0x3b9aca00",
                            "contractAddress": null,
                            "logs": [],
                            "logsBloom": format!("0x{}", "00".repeat(256)),
                            "status": "0x1",
                            "type": "0x2",
                        })),
                        other => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("not scripted: {other}")}}),
                    })
                },
            ),
        )
        .with_state(taking.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = chain::ChainConfig {
        label: "a node that signs".to_string(),
        rpc: url.clone(),
        ws: None,
        dollar: "USDC".to_string(),
        native_usd: None,
        confirmations: None,
        public_rpc: url,
        logs_rpc: None,
        log_range: None,
    };
    let client = Client::connect(&config, chain::Signing::Unlocked)
        .await
        .unwrap()
        .pipelined();
    receipts_polled_every(&client, Duration::from_millis(50));
    let client: Observed = client.into();
    let outcomes =
        futures::future::join_all((1..=3).map(|n| value(&client, keeper, vault(n)))).await;
    assert_eq!(outcomes, vec![Outcome::Done; 3]);
    let taking = taking.lock().unwrap();
    assert_eq!(taking.taken, 3);
    assert_eq!(
        taking.most, 1,
        "the node was handed two sends at once to number"
    );
}
