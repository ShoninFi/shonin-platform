//! Log queries: their own endpoints when the chain names some, a capped
//! width read in windows, and endpoints that no longer keep the past state
//! a read asks for passed over rather than taken at their word.
//!
//! Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, B256, U256};
use alloy::rpc::types::Filter;
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::{Value, json};
use stand_in::Said;

const SECRET: &str = "logs-key-0123456789abcdef";

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

/// A refusal, as a node gives it.
fn refusal(request: &Value, code: i64, message: &str) -> Said {
    Said::Raw(
        200,
        json!({"jsonrpc": "2.0", "id": request["id"].clone(), "error": {"code": code, "message": message}})
            .to_string(),
    )
}

/// The ranges asked for, and every method seen.
#[derive(Default)]
struct Seen {
    ranges: Vec<(u64, u64)>,
    methods: Vec<String>,
}

/// A node with one log in every block, refusing any log query wider than
/// `widest` blocks (when given), and answering a balance read with one.
fn logs_node(seen: Arc<Mutex<Seen>>, widest: Option<u64>) -> stand_in::Answer {
    Arc::new(move |method, request| {
        seen.lock().unwrap().methods.push(method.to_string());
        match method {
            "eth_getLogs" => {
                let from = number(&request["params"][0]["fromBlock"]);
                let to = number(&request["params"][0]["toBlock"]);
                seen.lock().unwrap().ranges.push((from, to));
                if widest.is_some_and(|widest| to - from + 1 > widest) {
                    return refusal(request, -32600, "block range too large");
                }
                Said::Result(Value::Array((from..=to).map(log_at).collect()))
            }
            "eth_blockNumber" => Said::Result(json!("0x3e8")),
            "eth_getBalance" => Said::Result(json!("0x1")),
            _ => stand_in::unknown_method(),
        }
    })
}

async fn client_with(rpc: &str, logs_rpc: Option<&str>, log_range: Option<u64>) -> chain::Client {
    let mut config = stand_in::config(rpc);
    config.logs_rpc = logs_rpc.map(str::to_string);
    config.log_range = log_range;
    chain::Client::connect(&config, Signing::ReadOnly)
        .await
        .unwrap()
}

fn blocks_of(logs: &[alloy::rpc::types::Log]) -> Vec<u64> {
    logs.iter().map(|log| log.block_number.unwrap()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capped_range_is_read_in_windows_covering_each_block_once_in_order() {
    let seen: Arc<Mutex<Seen>> = Default::default();
    let url = stand_in::answering(Some(31337), logs_node(seen.clone(), None)).await;
    let client = client_with(&url, None, Some(10)).await;

    let logs = client
        .logs(&Filter::new().from_block(100u64).to_block(134u64))
        .await
        .unwrap();
    assert_eq!(blocks_of(&logs), (100..=134).collect::<Vec<_>>());
    let mut ranges = seen.lock().unwrap().ranges.clone();
    ranges.sort();
    assert_eq!(ranges, vec![(100, 109), (110, 119), (120, 129), (130, 134)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capped_read_up_to_the_head_keeps_to_the_cap() {
    let seen: Arc<Mutex<Seen>> = Default::default();
    let url = stand_in::answering(Some(31337), logs_node(seen.clone(), Some(500))).await;
    let client = client_with(&url, None, Some(500)).await;
    // The head is block 1,000.
    let logs = client.logs(&Filter::new().from_block(1u64)).await.unwrap();
    assert_eq!(blocks_of(&logs), (1..=1000).collect::<Vec<_>>());
    let ranges = seen.lock().unwrap().ranges.clone();
    assert!(
        ranges.iter().all(|(from, to)| to - from < 500),
        "{ranges:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_cap_a_range_is_asked_for_once() {
    let seen: Arc<Mutex<Seen>> = Default::default();
    let url = stand_in::answering(Some(31337), logs_node(seen.clone(), None)).await;
    let client = client_with(&url, None, None).await;
    // The head is block 1,000.
    let logs = client
        .logs(&Filter::new().from_block(1u64).to_block(1_000u64))
        .await
        .unwrap();
    assert_eq!(logs.len(), 1_000);
    assert_eq!(seen.lock().unwrap().ranges, vec![(1, 1_000)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_window_still_refused_as_too_wide_is_halved() {
    let seen: Arc<Mutex<Seen>> = Default::default();
    // The cap says 100, and the endpoint takes no more than 30.
    let url = stand_in::answering(Some(31337), logs_node(seen.clone(), Some(30))).await;
    let client = client_with(&url, None, Some(100)).await;
    let logs = client
        .logs(&Filter::new().from_block(1u64).to_block(100u64))
        .await
        .unwrap();
    assert_eq!(blocks_of(&logs), (1..=100).collect::<Vec<_>>());
    let ranges = seen.lock().unwrap().ranges.clone();
    assert_eq!(ranges[0], (1, 100), "the window was asked for first");
    assert!(ranges.len() > 1, "{ranges:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_a_logs_list_log_queries_and_everything_else_keep_to_their_own_endpoints() {
    let main: Arc<Mutex<Seen>> = Default::default();
    let logs: Arc<Mutex<Seen>> = Default::default();
    let main_url = stand_in::answering(Some(31337), logs_node(main.clone(), None)).await;
    let logs_url = stand_in::answering(Some(31337), logs_node(logs.clone(), None)).await;
    let client = client_with(&main_url, Some(&logs_url), Some(50)).await;

    let found = client
        .logs(&Filter::new().from_block(1u64).to_block(120u64))
        .await
        .unwrap();
    assert_eq!(found.len(), 120);
    client.balance(Address::repeat_byte(0x33)).await.unwrap();
    client.block_number().await.unwrap();

    let main = main.lock().unwrap();
    let logs = logs.lock().unwrap();
    assert!(
        !main.methods.iter().any(|method| method == "eth_getLogs"),
        "a log query reached the main endpoints: {:?}",
        main.methods
    );
    assert!(
        // A log query, the head it is checked against, and the chain id
        // checked at connect.
        logs.methods.iter().all(|method| matches!(
            method.as_str(),
            "eth_getLogs" | "eth_blockNumber" | "eth_chainId"
        )),
        "something but a log query reached the logs endpoints: {:?}",
        logs.methods
    );
    assert!(main.methods.iter().any(|method| method == "eth_getBalance"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_logs_endpoint_on_another_chain_is_refused() {
    let main_url = stand_in::answering(Some(31337), logs_node(Default::default(), None)).await;
    let other = stand_in::answering(Some(8453), logs_node(Default::default(), None)).await;
    let mut config = stand_in::config(&main_url);
    config.logs_rpc = Some(other);
    let Err(error) = chain::Client::connect(&config, Signing::ReadOnly).await else {
        panic!("a logs endpoint on chain 8453 served chain 31337");
    };
    assert!(format!("{error:#}").contains("8453"), "{error:#}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keyed_logs_url_never_appears_in_an_error() {
    let answer: stand_in::Answer = Arc::new(move |method, request| match method {
        "eth_getLogs" => refusal(
            request,
            -32000,
            &format!("no project at /v2/{SECRET} for this query"),
        ),
        _ => stand_in::unknown_method(),
    });
    let main_url = stand_in::answering(Some(31337), logs_node(Default::default(), None)).await;
    let logs_url = stand_in::answering(Some(31337), answer).await;
    // One keyed endpoint, and a list of two.
    for list in [
        format!("{logs_url}/v2/{SECRET}"),
        format!("{logs_url}/v2/{SECRET} {logs_url}/other/{SECRET}"),
    ] {
        let client = client_with(&main_url, Some(&list), Some(10)).await;
        let error = client
            .logs(&Filter::new().from_block(1u64).to_block(5u64))
            .await
            .expect_err("the endpoint refused");
        let text = format!("{error:#} / {error:?}");
        assert!(!text.contains(SECRET), "the error carries the key: {text}");
    }
}

/// A node that has pruned the past: a read of anything but the head is
/// refused with `message`, and a send is refused with it too.
fn pruned_node(seen: Arc<Mutex<Seen>>, code: i64, message: &'static str) -> stand_in::Answer {
    Arc::new(move |method, request| {
        seen.lock().unwrap().methods.push(method.to_string());
        match method {
            "eth_call" | "eth_getBalance" | "eth_sendRawTransaction" => {
                refusal(request, code, message)
            }
            _ => stand_in::unknown_method(),
        }
    })
}

/// A node that keeps the past.
fn archive_node(seen: Arc<Mutex<Seen>>) -> stand_in::Answer {
    Arc::new(move |method, _request| {
        seen.lock().unwrap().methods.push(method.to_string());
        match method {
            "eth_call" => Said::Result(json!(format!("0x{}", "00".repeat(31) + "07"))),
            "eth_getBalance" => Said::Result(json!("0x5")),
            "eth_sendRawTransaction" => Said::Result(json!(B256::repeat_byte(0x99))),
            _ => stand_in::unknown_method(),
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_an_endpoint_has_pruned_moves_on_to_one_that_keeps_it() {
    for message in [
        "historical state 0c1857f57b3ecc4ebf8d1889d12e66d39d324e81518720fb83598755297c268b is not available",
        "missing trie node 0c18 (path ) state 0x0c18 is not available, not found",
        "state at block #45748311 is pruned",
        "Unknown state. First available state is 1",
        "no state found for block 0x5",
    ] {
        let pruned: Arc<Mutex<Seen>> = Default::default();
        let archive: Arc<Mutex<Seen>> = Default::default();
        let first =
            stand_in::answering(Some(31337), pruned_node(pruned.clone(), -32000, message)).await;
        let second = stand_in::answering(Some(31337), archive_node(archive.clone())).await;
        let client = client_with(&format!("{first} {second}"), None, None).await;

        let supply = client
            .read_at(
                Address::repeat_byte(0x22),
                IERC20::balanceOfCall {
                    account: Address::ZERO,
                },
                alloy::eips::BlockId::number(5),
            )
            .await
            .unwrap_or_else(|error| panic!("{message}: {error:#}"));
        assert_eq!(supply, U256::from(7u64), "{message}");
        assert!(
            pruned
                .lock()
                .unwrap()
                .methods
                .contains(&"eth_call".to_string()),
            "the pruned endpoint was asked first"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revert_is_the_answer_and_is_not_asked_again_elsewhere() {
    let first_seen: Arc<Mutex<Seen>> = Default::default();
    let archive: Arc<Mutex<Seen>> = Default::default();
    // A revert whose reason happens to read like missing history is still
    // the chain's answer.
    let first = stand_in::answering(
        Some(31337),
        pruned_node(first_seen.clone(), 3, "execution reverted: state is pruned"),
    )
    .await;
    let second = stand_in::answering(Some(31337), archive_node(archive.clone())).await;
    let client = client_with(&format!("{first} {second}"), None, None).await;
    let error = client
        .read(
            Address::repeat_byte(0x22),
            IERC20::balanceOfCall {
                account: Address::ZERO,
            },
        )
        .await
        .expect_err("the call reverted");
    assert!(format!("{error:#}").contains("reverted"), "{error:#}");
    assert!(
        !archive
            .lock()
            .unwrap()
            .methods
            .contains(&"eth_call".to_string()),
        "a revert was asked again of another endpoint"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_refused_in_missing_history_s_words_is_never_sent_to_a_second_endpoint() {
    let first_seen: Arc<Mutex<Seen>> = Default::default();
    let archive: Arc<Mutex<Seen>> = Default::default();
    let first = stand_in::answering(
        Some(31337),
        pruned_node(first_seen.clone(), -32000, "missing trie node 0c18"),
    )
    .await;
    let second = stand_in::answering(Some(31337), archive_node(archive.clone())).await;
    let client = client_with(&format!("{first} {second}"), None, None).await;
    let refused = client
        .raw_request("eth_sendRawTransaction", json!(["0x02"]))
        .await;
    assert!(refused.is_err(), "the first endpoint refused it");
    assert!(
        !archive
            .lock()
            .unwrap()
            .methods
            .contains(&"eth_sendRawTransaction".to_string()),
        "the send went to a second endpoint"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_no_endpoint_has_fails_rather_than_answering() {
    let a = stand_in::answering(
        Some(31337),
        pruned_node(
            Default::default(),
            -32000,
            "historical state 0c18 is not available",
        ),
    )
    .await;
    let b = stand_in::answering(
        Some(31337),
        pruned_node(Default::default(), -32603, "state at block #5 is pruned"),
    )
    .await;
    let client = client_with(&format!("{a} {b}"), None, None).await;
    let error = client
        .read_at(
            Address::repeat_byte(0x22),
            IERC20::balanceOfCall {
                account: Address::ZERO,
            },
            alloy::eips::BlockId::number(5),
        )
        .await
        .expect_err("no endpoint keeps block 5");
    assert!(chain::node_trouble(&error), "{error:#}");
}

/// A node that answers every log query with an HTTP 429 and a body in
/// words a width refusal might also use.
fn limiting_node(seen: Arc<Mutex<Seen>>) -> stand_in::Answer {
    Arc::new(move |method, request| {
        seen.lock().unwrap().methods.push(method.to_string());
        match method {
            "eth_getLogs" => {
                let from = number(&request["params"][0]["fromBlock"]);
                let to = number(&request["params"][0]["toBlock"]);
                seen.lock().unwrap().ranges.push((from, to));
                Said::Raw(
                    429,
                    "Too many requests in this range, slow down".to_string(),
                )
            }
            "eth_blockNumber" => Said::Result(json!("0x3e8")),
            _ => stand_in::unknown_method(),
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rate_limit_on_a_log_query_moves_to_the_next_endpoint_and_is_never_halved() {
    let (limited, open): (Arc<Mutex<Seen>>, Arc<Mutex<Seen>>) = Default::default();
    let main_url = stand_in::answering(Some(31337), logs_node(Default::default(), None)).await;
    let first = stand_in::answering(Some(31337), limiting_node(limited.clone())).await;
    let second = stand_in::answering(Some(31337), logs_node(open.clone(), None)).await;
    let client = client_with(&main_url, Some(&format!("{first} {second}")), None).await;

    let logs = client
        .logs(&Filter::new().from_block(1u64).to_block(100u64))
        .await
        .unwrap();
    assert_eq!(blocks_of(&logs), (1..=100).collect::<Vec<_>>());
    assert_eq!(limited.lock().unwrap().ranges, vec![(1, 100)], "halved");
    assert_eq!(open.lock().unwrap().ranges, vec![(1, 100)]);

    // With only the limiting endpoint, the read is asked again a few times
    // (every endpoint of the list limited the rate), then fails as the
    // endpoint's trouble, and still unhalved.
    let alone: Arc<Mutex<Seen>> = Default::default();
    let only = stand_in::answering(Some(31337), limiting_node(alone.clone())).await;
    let client = client_with(&main_url, Some(&only), None).await;
    let error = client
        .logs(&Filter::new().from_block(1u64).to_block(100u64))
        .await
        .expect_err("the only endpoint limits the rate");
    assert!(chain::node_trouble(&error), "{error:#}");
    assert_eq!(alone.lock().unwrap().ranges, vec![(1, 100); 4], "halved");
}

/// A refusal a log node gives: its code and message.
type Refused = Option<(i64, &'static str)>;

/// A node with one log in every block up to its head at `head`, refusing a
/// log query as `refuses` says, given the range and every range asked
/// before it.
fn log_node_refusing(
    seen: Arc<Mutex<Seen>>,
    head: u64,
    refuses: impl Fn(u64, u64, &[(u64, u64)]) -> Refused + Send + Sync + 'static,
) -> stand_in::Answer {
    Arc::new(move |method, request| {
        seen.lock().unwrap().methods.push(method.to_string());
        match method {
            "eth_getLogs" => {
                let from = number(&request["params"][0]["fromBlock"]);
                let to = number(&request["params"][0]["toBlock"]);
                let refused = {
                    let mut seen = seen.lock().unwrap();
                    let refused = refuses(from, to, &seen.ranges);
                    seen.ranges.push((from, to));
                    refused
                };
                if let Some((code, message)) = refused {
                    return refusal(request, code, message);
                }
                Said::Result(Value::Array((from..=to).map(log_at).collect()))
            }
            "eth_blockNumber" => Said::Result(json!(format!("0x{head:x}"))),
            _ => stand_in::unknown_method(),
        }
    })
}

const INVALID_RANGE: (i64, &str) = (-32602, "invalid block range");
const TIMED_OUT: (i64, &str) = (-32000, "execution aborted (timeout = 10s)");
const LIMITED: (i64, &str) = (-32005, "rate limited");

/// A node that no longer keeps the logs of blocks below `kept_from`: any
/// log query reaching below it is refused as `refusal` says, whatever its
/// width, as some providers refuse a block they have dropped.
fn forgetting_node(
    seen: Arc<Mutex<Seen>>,
    kept_from: u64,
    refusal: (i64, &'static str),
) -> stand_in::Answer {
    log_node_refusing(seen, 1000, move |from, _, _| {
        (from < kept_from).then_some(refusal)
    })
}

/// A node that keeps every block up to its head at `head`, and limits the
/// rate of its first log query.
fn archive_node_limiting_its_first_log_query(
    seen: Arc<Mutex<Seen>>,
    head: u64,
) -> stand_in::Answer {
    log_node_refusing(seen, head, |_, _, before| {
        before.is_empty().then_some(LIMITED)
    })
}

/// A node that keeps every block, refuses a log query wider than `widest`
/// blocks for its width, and limits the rate of each range the first time
/// it is asked for it.
fn capped_archive_node_limiting_each_range_once(
    seen: Arc<Mutex<Seen>>,
    widest: u64,
) -> stand_in::Answer {
    log_node_refusing(seen, 1000, move |from, to, before| {
        if to - from + 1 > widest {
            Some((-32600, "query exceeds max block range"))
        } else if !before.contains(&(from, to)) {
            Some(LIMITED)
        } else {
            None
        }
    })
}

/// Where nothing listens.
async fn out_of_reach() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{address}")
}

/// A client whose log queries go to `nodes` alone, in that order.
async fn client_reading_logs_from(nodes: &[String], log_range: Option<u64>) -> chain::Client {
    let main_url = stand_in::answering(Some(31337), logs_node(Default::default(), None)).await;
    client_with(&main_url, Some(&nodes.join(" ")), log_range).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_block_an_endpoint_no_longer_keeps_is_read_from_another_without_narrowing_the_first() {
    for refused in [INVALID_RANGE, TIMED_OUT] {
        let message = refused.1;
        let (forgetting, archive): (Arc<Mutex<Seen>>, Arc<Mutex<Seen>>) = Default::default();
        let client = client_reading_logs_from(
            &[
                stand_in::answering(
                    Some(31337),
                    forgetting_node(forgetting.clone(), 500, refused),
                )
                .await,
                stand_in::answering(
                    Some(31337),
                    archive_node_limiting_its_first_log_query(archive.clone(), 499),
                )
                .await,
            ],
            None,
        )
        .await;

        // The first endpoint has dropped these blocks and the second limits
        // the rate once: the window waits for the second, which has them.
        let logs = client
            .logs(&Filter::new().from_block(1u64).to_block(100u64))
            .await
            .unwrap_or_else(|error| panic!("{message}: {error:#}"));
        assert_eq!(blocks_of(&logs), (1..=100).collect::<Vec<_>>(), "{message}");

        // Blocks the first endpoint keeps, and the second hasn't reached,
        // are asked of the first whole: dropping a block taught nothing of
        // the width it takes.
        let logs = client
            .logs(&Filter::new().from_block(600u64).to_block(700u64))
            .await
            .unwrap_or_else(|error| panic!("{message}: {error:#}"));
        assert_eq!(
            blocks_of(&logs),
            (600..=700).collect::<Vec<_>>(),
            "{message}"
        );
        let ranges = forgetting.lock().unwrap().ranges.clone();
        assert_eq!(ranges.last(), Some(&(600, 700)), "{message}: {ranges:?}");
        assert!(
            ranges.iter().filter(|(from, _)| *from >= 500).count() == 1,
            "{message}: {ranges:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_block_no_endpoint_keeps_fails_once_each_endpoint_has_been_asked() {
    let (one, two): (Arc<Mutex<Seen>>, Arc<Mutex<Seen>>) = Default::default();
    let client = client_reading_logs_from(
        &[
            stand_in::answering(
                Some(31337),
                forgetting_node(one.clone(), 500, INVALID_RANGE),
            )
            .await,
            stand_in::answering(
                Some(31337),
                forgetting_node(two.clone(), 500, INVALID_RANGE),
            )
            .await,
        ],
        None,
    )
    .await;
    let error = client
        .logs(&Filter::new().from_block(1u64).to_block(100u64))
        .await
        .expect_err("no endpoint keeps these blocks");
    assert!(
        format!("{error:#}").contains("can't serve block 1"),
        "{error:#}"
    );
    // Each was asked the window and its first block, once: a block it
    // doesn't keep is never halved towards.
    for seen in [one, two] {
        assert_eq!(seen.lock().unwrap().ranges, vec![(1, 100), (1, 1)]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_range_refused_as_invalid_whose_first_block_answers_is_halved_and_its_width_kept() {
    let seen: Arc<Mutex<Seen>> = Default::default();
    let node = log_node_refusing(seen.clone(), 1000, |from, to, _| {
        (to - from + 1 > 50).then_some(INVALID_RANGE)
    });
    let client =
        client_reading_logs_from(&[stand_in::answering(Some(31337), node).await], None).await;
    for (from, to) in [(1u64, 100u64), (101, 200)] {
        let logs = client
            .logs(&Filter::new().from_block(from).to_block(to))
            .await
            .unwrap();
        assert_eq!(blocks_of(&logs), (from..=to).collect::<Vec<_>>());
    }
    // The second window is read in the width the first showed it takes.
    assert_eq!(
        seen.lock().unwrap().ranges,
        vec![(1, 100), (1, 1), (1, 50), (51, 100), (101, 150), (151, 200)]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rate_limited_check_of_the_first_block_teaches_nothing_and_is_waited_out() {
    let seen: Arc<Mutex<Seen>> = Default::default();
    let node = log_node_refusing(seen.clone(), 1000, |from, to, before| {
        if to - from + 1 > 50 {
            Some(INVALID_RANGE)
        } else if from == to && !before.contains(&(from, to)) {
            Some(LIMITED)
        } else {
            None
        }
    });
    let client =
        client_reading_logs_from(&[stand_in::answering(Some(31337), node).await], None).await;
    let logs = client
        .logs(&Filter::new().from_block(1u64).to_block(100u64))
        .await
        .unwrap();
    assert_eq!(blocks_of(&logs), (1..=100).collect::<Vec<_>>());
    // Nothing was halved until the first block answered.
    assert_eq!(
        seen.lock().unwrap().ranges,
        vec![(1, 100), (1, 1), (1, 100), (1, 1), (1, 50), (51, 100)]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_time_out_on_a_wide_range_halves_that_read_alone() {
    let seen: Arc<Mutex<Seen>> = Default::default();
    // Slow once, at the width of the first window: the node is loaded,
    // not capped.
    let node = log_node_refusing(seen.clone(), 1000, |from, to, before| {
        (to - from + 1 > 50 && before.is_empty()).then_some(TIMED_OUT)
    });
    let client =
        client_reading_logs_from(&[stand_in::answering(Some(31337), node).await], None).await;
    for (from, to) in [(1u64, 100u64), (101, 200)] {
        let logs = client
            .logs(&Filter::new().from_block(from).to_block(to))
            .await
            .unwrap();
        assert_eq!(blocks_of(&logs), (from..=to).collect::<Vec<_>>());
    }
    assert_eq!(
        seen.lock().unwrap().ranges,
        vec![(1, 100), (1, 1), (1, 50), (51, 100), (101, 200)]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_block_refused_part_way_through_a_window_returns_none_of_the_parts_before_it() {
    let seen: Arc<Mutex<Seen>> = Default::default();
    // Wider than 50 blocks is too wide; block 60 it can't serve.
    let node = log_node_refusing(seen.clone(), 1000, |from, to, _| {
        if to - from + 1 > 50 {
            Some((-32600, "block range too large"))
        } else if (from..=to).contains(&60) {
            Some(INVALID_RANGE)
        } else {
            None
        }
    });
    let client =
        client_reading_logs_from(&[stand_in::answering(Some(31337), node).await], None).await;
    let error = client
        .logs(&Filter::new().from_block(1u64).to_block(100u64))
        .await
        .expect_err("block 60 is served nowhere");
    assert!(
        format!("{error:#}").contains("can't serve block 60"),
        "{error:#}"
    );
    assert!(seen.lock().unwrap().ranges.contains(&(1, 50)), "read first");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_single_block_refused_for_its_logs_is_not_reported_as_one_the_endpoint_cannot_serve() {
    let node = log_node_refusing(Default::default(), 1000, |_, _, _| {
        Some((-32600, "query returned more than 10000 results"))
    });
    let client =
        client_reading_logs_from(&[stand_in::answering(Some(31337), node).await], None).await;
    let error = client
        .logs(&Filter::new().from_block(7u64).to_block(7u64))
        .await
        .expect_err("refused");
    let error = format!("{error:#}");
    assert!(error.contains("more than 10000 results"), "{error}");
    assert!(!error.contains("can't serve"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_over_several_windows_waits_out_the_endpoint_limiting_the_rate_past_those_that_cannot_serve_it()
 {
    let (forgetting, archive): (Arc<Mutex<Seen>>, Arc<Mutex<Seen>>) = Default::default();
    let client = client_reading_logs_from(
        &[
            stand_in::answering(
                Some(31337),
                forgetting_node(forgetting, 10_000, INVALID_RANGE),
            )
            .await,
            stand_in::answering(
                Some(31337),
                capped_archive_node_limiting_each_range_once(archive.clone(), 50),
            )
            .await,
            out_of_reach().await,
        ],
        Some(100),
    )
    .await;
    let logs = client
        .logs(&Filter::new().from_block(1u64).to_block(400u64))
        .await
        .unwrap();
    assert_eq!(blocks_of(&logs), (1..=400).collect::<Vec<_>>());
    let ranges = archive.lock().unwrap().ranges.clone();
    assert!(
        ranges.iter().all(|(from, to)| to - from < 100),
        "{ranges:?}"
    );
}
