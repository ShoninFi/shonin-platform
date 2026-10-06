//! A log read is complete only when the endpoint that answered it had
//! reached the range's last block.
//!
//! Every reader (the keeper, the executor's watch, the record) takes the
//! head from the chain's main endpoints, reads the logs up to it, and moves
//! its cursor past what it read. With endpoints of their own for log
//! queries, the head and the logs come from different providers, and the
//! one serving logs may be a few blocks behind the one that gave the head.
//! Most node software answers a range that runs past its own head with the
//! logs it has, without an error. Taken as the whole answer, the blocks it
//! hadn't reached are never read: the cursor has moved past them.
//!
//! So a window of a log read goes only to an endpoint whose own head has
//! reached its end, and is asked again a few times when none has.
//!
//! Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, B256, U256};
use alloy::rpc::types::Filter;
use chain::Signing;
use serde_json::{Value, json};
use stand_in::Said;

fn number(value: &Value) -> u64 {
    u64::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

/// One log at `block`, as a node gives it.
fn log_at(block: u64) -> Value {
    json!({
        "address": Address::repeat_byte(0x22),
        "topics": [B256::repeat_byte(0x01)],
        "data": "0x",
        "blockNumber": format!("0x{block:x}"),
        "blockHash": B256::repeat_byte(0xbb),
        "transactionHash": B256::from(U256::from(block)),
        "transactionIndex": "0x0",
        "logIndex": "0x0",
        "removed": false,
    })
}

/// A node at `head`, with one log in every block it has reached. A log
/// query running past its head is answered with the logs up to its head and
/// no error, as geth-family nodes answer it.
fn node_at(head: Arc<AtomicU64>) -> stand_in::Answer {
    Arc::new(move |method, request| {
        let head = head.load(Ordering::SeqCst);
        match method {
            "eth_blockNumber" => Said::Result(json!(format!("0x{head:x}"))),
            "eth_getLogs" => {
                let from = number(&request["params"][0]["fromBlock"]);
                let to = number(&request["params"][0]["toBlock"]).min(head);
                Said::Result(Value::Array((from..=to).map(log_at).collect()))
            }
            _ => stand_in::unknown_method(),
        }
    })
}

/// Read as the services' readers do: the head from the client, the logs
/// from the cursor up to it, and the cursor moved past the head when the
/// read is answered. A read that fails leaves the cursor where it was.
async fn read_on(client: &chain::Client, cursor: &mut u64, seen: &mut Vec<u64>) {
    let head = client.block_number().await.unwrap();
    if head < *cursor {
        return;
    }
    if let Ok(logs) = client
        .logs(&Filter::new().from_block(*cursor).to_block(head))
        .await
    {
        seen.extend(logs.iter().map(|log| log.block_number.unwrap()));
        *cursor = head + 1;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocks_the_log_endpoint_had_not_reached_are_still_read() {
    for log_range in [None, Some(4)] {
        let main_head = Arc::new(AtomicU64::new(105));
        let logs_head = Arc::new(AtomicU64::new(100));
        let main_url = stand_in::answering(Some(31337), node_at(main_head.clone())).await;
        let logs_url = stand_in::answering(Some(31337), node_at(logs_head.clone())).await;
        let mut config = stand_in::config(&main_url);
        config.logs_rpc = Some(logs_url);
        config.log_range = log_range;
        let client = chain::Client::connect(&config, Signing::ReadOnly)
            .await
            .unwrap();

        let mut cursor = 95;
        let mut seen = Vec::new();
        // The main endpoints are at block 105, and the one serving logs has
        // reached 100.
        read_on(&client, &mut cursor, &mut seen).await;
        // Both move on, the one serving logs catching up, and the reader
        // keeps reading for a few rounds.
        for head in [110, 110, 110] {
            main_head.store(head, Ordering::SeqCst);
            logs_head.store(head, Ordering::SeqCst);
            read_on(&client, &mut cursor, &mut seen).await;
        }

        let missed: Vec<u64> = (95..=110).filter(|block| !seen.contains(block)).collect();
        assert!(
            missed.is_empty(),
            "with a width cap of {log_range:?}, the logs of blocks {missed:?} were never read: \
             the read up to 105 was answered by an endpoint at 100 and taken as complete"
        );
    }
}

/// The log ranges a node was asked for.
type Asked = Arc<Mutex<Vec<(u64, u64)>>>;

/// [`node_at`], noting each log range it is asked for.
fn noting_node_at(head: u64, asked: Asked) -> stand_in::Answer {
    let inner = node_at(Arc::new(AtomicU64::new(head)));
    Arc::new(move |method, request| {
        if method == "eth_getLogs" {
            asked.lock().unwrap().push((
                number(&request["params"][0]["fromBlock"]),
                number(&request["params"][0]["toBlock"]),
            ));
        }
        inner(method, request)
    })
}

async fn reading_logs_from(logs_rpc: String, log_range: Option<u64>) -> chain::Client {
    let main = stand_in::answering(Some(31337), node_at(Arc::new(AtomicU64::new(105)))).await;
    let mut config = stand_in::config(&main);
    config.logs_rpc = Some(logs_rpc);
    config.log_range = log_range;
    chain::Client::connect(&config, Signing::ReadOnly)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_endpoint_short_of_the_range_is_passed_over_for_one_that_has_it() {
    for log_range in [None, Some(4)] {
        let (behind, current): (Asked, Asked) = Default::default();
        // Listed first, and so asked first: one that clamps silently at 100.
        let first = stand_in::answering(Some(31337), noting_node_at(100, behind.clone())).await;
        let second = stand_in::answering(Some(31337), noting_node_at(105, current.clone())).await;
        let client = reading_logs_from(format!("{first} {second}"), log_range).await;

        let logs = client
            .logs(&Filter::new().from_block(95u64).to_block(105u64))
            .await
            .unwrap();
        let blocks: Vec<u64> = logs.iter().map(|log| log.block_number.unwrap()).collect();
        assert_eq!(blocks, (95..=105).collect::<Vec<_>>(), "cap {log_range:?}");
        assert!(
            behind.lock().unwrap().iter().all(|(_, to)| *to <= 100),
            "a window past its head was asked of the endpoint at 100: {:?}",
            behind.lock().unwrap()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_every_endpoint_short_of_the_range_the_read_fails_after_its_tries() {
    let asked: Asked = Default::default();
    let heads_asked = Arc::new(AtomicU64::new(0));
    let behind = {
        let inner = noting_node_at(100, asked.clone());
        let heads_asked = heads_asked.clone();
        let answer: stand_in::Answer = Arc::new(move |method, request| {
            if method == "eth_blockNumber" {
                heads_asked.fetch_add(1, Ordering::SeqCst);
            }
            inner(method, request)
        });
        stand_in::answering(Some(31337), answer).await
    };
    let client = reading_logs_from(behind, None).await;
    let error = client
        .logs(&Filter::new().from_block(95u64).to_block(105u64))
        .await
        .expect_err("no endpoint has reached block 105");
    assert!(format!("{error:#}").contains("105"), "{error:#}");
    assert!(
        asked.lock().unwrap().is_empty(),
        "a range the endpoint hasn't reached was asked of it"
    );
    assert!(
        heads_asked.load(Ordering::SeqCst) > 1,
        "the read gave up without asking again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_endpoint_that_has_the_range_answers_it_whole() {
    let asked: Asked = Default::default();
    let current = stand_in::answering(Some(31337), noting_node_at(105, asked.clone())).await;
    let client = reading_logs_from(current, None).await;
    let logs = client
        .logs(&Filter::new().from_block(95u64).to_block(105u64))
        .await
        .unwrap();
    assert_eq!(logs.len(), 11);
    assert_eq!(*asked.lock().unwrap(), vec![(95, 105)]);
}
