//! A log window goes whole to an endpoint that takes it, and an endpoint
//! known to refuse a width is not asked that wide again.
//!
//! The free endpoints of one chain cap a log query's width very
//! differently (one takes about a hundred blocks, another a thousand), and
//! say so only by refusing. Halved at whichever endpoint happened to be
//! asked first, every window of a long catch-up is read in dozens of
//! queries, each refused width asked again, while another endpoint would
//! have taken it in one: a first read of a chain's history then takes the
//! better part of an hour.
//!
//! Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, B256, U256};
use alloy::rpc::types::Filter;
use chain::Signing;
use serde_json::{Value, json};
use stand_in::Said;

fn number(value: &Value) -> u64 {
    u64::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

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

/// A node at block 1,000 with one log in every block, refusing any log
/// query wider than `widest` blocks as a free plan does; every range it is
/// asked for is kept, taken or not.
fn capped_node(asked: Arc<Mutex<Vec<(u64, u64)>>>, widest: u64) -> stand_in::Answer {
    Arc::new(move |method, request| match method {
        "eth_getLogs" => {
            let from = number(&request["params"][0]["fromBlock"]);
            let to = number(&request["params"][0]["toBlock"]);
            asked.lock().unwrap().push((from, to));
            if to - from + 1 > widest {
                return Said::Raw(
                    200,
                    json!({
                        "jsonrpc": "2.0",
                        "id": request["id"].clone(),
                        "error": {"code": 35, "message": "ranges over 10000 blocks are not supported on free plan"},
                    })
                    .to_string(),
                );
            }
            Said::Result(Value::Array((from..=to).map(log_at).collect()))
        }
        "eth_blockNumber" => Said::Result(json!("0x3e8")),
        _ => stand_in::unknown_method(),
    })
}

async fn client_reading_logs_from(list: &str) -> chain::Client {
    let main = stand_in::answering(Some(31337), capped_node(Default::default(), u64::MAX)).await;
    let mut config = stand_in::config(&main);
    config.logs_rpc = Some(list.to_string());
    chain::Client::connect(&config, Signing::ReadOnly)
        .await
        .unwrap()
}

fn blocks_of(logs: &[alloy::rpc::types::Log]) -> Vec<u64> {
    logs.iter().map(|log| log.block_number.unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_window_one_endpoint_refuses_goes_whole_to_one_that_takes_it() {
    let (narrow, wide): (Arc<Mutex<_>>, Arc<Mutex<_>>) = Default::default();
    // The narrow one is first, so asked first.
    let first = stand_in::answering(Some(31337), capped_node(narrow.clone(), 10)).await;
    let second = stand_in::answering(Some(31337), capped_node(wide.clone(), 500)).await;
    let client = client_reading_logs_from(&format!("{first} {second}")).await;

    for (from, to) in [(1u64, 200u64), (201, 400), (401, 600)] {
        let logs = client
            .logs(&Filter::new().from_block(from).to_block(to))
            .await
            .unwrap();
        assert_eq!(blocks_of(&logs), (from..=to).collect::<Vec<_>>());
    }
    assert_eq!(
        *wide.lock().unwrap(),
        vec![(1, 200), (201, 400), (401, 600)],
        "each window was read whole by the endpoint that takes it"
    );
    assert_eq!(
        *narrow.lock().unwrap(),
        vec![(1, 200)],
        "the endpoint that refused the width was asked it again, or made to read it in parts"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_endpoint_that_refused_a_width_reads_later_windows_in_parts_it_takes() {
    let asked: Arc<Mutex<Vec<(u64, u64)>>> = Default::default();
    let only = stand_in::answering(Some(31337), capped_node(asked.clone(), 30)).await;
    let client = client_reading_logs_from(&only).await;

    let logs = client
        .logs(&Filter::new().from_block(1u64).to_block(100u64))
        .await
        .unwrap();
    assert_eq!(blocks_of(&logs), (1..=100).collect::<Vec<_>>());
    let first = std::mem::take(&mut *asked.lock().unwrap());
    assert_eq!(first[0], (1, 100), "the window was asked for whole first");

    let logs = client
        .logs(&Filter::new().from_block(101u64).to_block(200u64))
        .await
        .unwrap();
    assert_eq!(blocks_of(&logs), (101..=200).collect::<Vec<_>>());
    let later = asked.lock().unwrap().clone();
    assert!(
        later.iter().all(|(from, to)| to - from < 30),
        "a width it refused was asked again: {later:?}"
    );
    // In parts as wide as the widest it took (25, from halving 100), not
    // halved further.
    assert_eq!(later.len(), 4, "{later:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_widest_endpoint_reads_a_window_every_endpoint_refuses_once_widths_are_known() {
    let (narrow, wider): (Arc<Mutex<_>>, Arc<Mutex<_>>) = Default::default();
    // The narrow one is first, so asked first; neither takes 200 blocks.
    let first = stand_in::answering(Some(31337), capped_node(narrow.clone(), 10)).await;
    let second = stand_in::answering(Some(31337), capped_node(wider.clone(), 50)).await;
    let client = client_reading_logs_from(&format!("{first} {second}")).await;

    // Each refuses a width once; what each takes is found as they read.
    for (from, to) in [(1u64, 200u64), (201, 400), (401, 600), (601, 800)] {
        let logs = client
            .logs(&Filter::new().from_block(from).to_block(to))
            .await
            .unwrap();
        assert_eq!(blocks_of(&logs), (from..=to).collect::<Vec<_>>());
    }
    let after = |asked: &Arc<Mutex<Vec<(u64, u64)>>>| -> Vec<(u64, u64)> {
        let asked = asked.lock().unwrap();
        asked
            .iter()
            .filter(|(from, _)| *from > 400)
            .copied()
            .collect()
    };
    assert_eq!(after(&narrow), vec![], "the narrower endpoint was asked");
    assert_eq!(
        after(&wider),
        vec![
            (401, 450),
            (451, 500),
            (501, 550),
            (551, 600),
            (601, 650),
            (651, 700),
            (701, 750),
            (751, 800)
        ],
    );
}
