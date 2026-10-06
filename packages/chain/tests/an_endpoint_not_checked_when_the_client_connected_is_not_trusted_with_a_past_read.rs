//! An endpoint the client could not check when it connected is not trusted,
//! for the client's whole life, to answer a read at a past block as of that
//! block.
//!
//! Some endpoints answer a read at any block with the head's state and no
//! error. The client finds them out when it connects, by asking each about
//! a recent transaction's sender. That one look can come to nothing for
//! reasons that pass: the endpoint was limiting the rate of requests while
//! every service started at once, or the chain had no transaction in the
//! few blocks looked through. An endpoint left unchecked then is asked every
//! past read from there on, first if it is first in the list, and one that
//! ignores the block answers the record's history, and a reading that must
//! belong to one block, with today's values.
//!
//! Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use alloy::eips::BlockId;
use alloy::primitives::{Address, B256, U256};
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::{Value, json};
use stand_in::{Past, Said};

const HEAD: u64 = 1_000;

/// What the contract holds at the head, and what it held at the past block
/// the tests read.
const AT_THE_HEAD: u64 = 99;
const AT_THE_PAST_BLOCK: u64 = 7;

fn number(value: &Value) -> Option<u64> {
    u64::from_str_radix(value.as_str()?.strip_prefix("0x")?, 16).ok()
}

fn word(value: u64) -> Said {
    Said::Result(json!(B256::from(U256::from(value))))
}

fn balance_of() -> IERC20::balanceOfCall {
    IERC20::balanceOfCall {
        account: Address::ZERO,
    }
}

/// The blocks each contract read was asked at (`None` for the head).
type Reads = Arc<Mutex<Vec<Option<u64>>>>;

/// An endpoint of one chain. `past` says how it answers a read of state at a
/// past block. While `limiting` is set it limits the rate of everything but
/// the chain id. Blocks above `quiet_above` hold no transaction.
fn endpoint(
    past: Past,
    limiting: Arc<AtomicBool>,
    quiet_above: u64,
    reads: Reads,
) -> stand_in::Answer {
    Arc::new(move |method, request| {
        if limiting.load(Ordering::SeqCst) {
            return Said::Raw(429, "Too Many Requests".to_string());
        }
        if method == "eth_getBlockByNumber"
            && number(&request["params"][0]).is_some_and(|block| block > quiet_above)
        {
            return Said::Result(json!({
                "number": request["params"][0].clone(),
                "transactions": [],
            }));
        }
        if let Some(said) = stand_in::past_block_check(method, request, past) {
            return said;
        }
        match method {
            "eth_blockNumber" => Said::Result(json!(format!("0x{HEAD:x}"))),
            "eth_call" => {
                let block = number(&request["params"][1]);
                reads.lock().unwrap().push(block);
                word(match (block, past) {
                    (Some(_), Past::Honours) => AT_THE_PAST_BLOCK,
                    _ => AT_THE_HEAD,
                })
            }
            _ => stand_in::unknown_method(),
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_endpoint_limiting_the_rate_when_the_client_connected_does_not_answer_the_past_with_the_head()
 {
    let (ignoring, honest): (Reads, Reads) = Default::default();
    let starting = Arc::new(AtomicBool::new(true));
    // First in the list: the endpoint that ignores the block, which limits
    // the rate while the client connects (a whole stack starting at once).
    let first = stand_in::answering(
        Some(31337),
        endpoint(Past::Ignores, starting.clone(), HEAD, ignoring.clone()),
    )
    .await;
    let second = stand_in::answering(
        Some(31337),
        endpoint(
            Past::Honours,
            Arc::new(AtomicBool::new(false)),
            HEAD,
            honest.clone(),
        ),
    )
    .await;
    let client = chain::Client::connect(
        &stand_in::config(&format!("{first} {second}")),
        Signing::ReadOnly,
    )
    .await
    .unwrap();
    // The stack has started; the endpoint answers again.
    starting.store(false, Ordering::SeqCst);

    for _ in 0..3 {
        let read = client
            .read_at(Address::repeat_byte(0x22), balance_of(), BlockId::number(5))
            .await;
        assert_ne!(
            read.ok(),
            Some(U256::from(AT_THE_HEAD)),
            "a read at block 5 was answered with the head's state"
        );
    }
    assert!(
        ignoring.lock().unwrap().iter().all(Option::is_none),
        "a past read was sent to an endpoint that ignores the block and was never checked"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_quiet_stretch_below_the_head_when_the_client_connected_does_not_let_the_head_answer_the_past()
 {
    let (ignoring, honest): (Reads, Reads) = Default::default();
    let never = || Arc::new(AtomicBool::new(false));
    // The chain's last hundred blocks hold no transaction; older ones do.
    let quiet_above = HEAD - 100;
    let first = stand_in::answering(
        Some(31337),
        endpoint(Past::Ignores, never(), quiet_above, ignoring.clone()),
    )
    .await;
    let second = stand_in::answering(
        Some(31337),
        endpoint(Past::Honours, never(), quiet_above, honest.clone()),
    )
    .await;
    let client = chain::Client::connect(
        &stand_in::config(&format!("{first} {second}")),
        Signing::ReadOnly,
    )
    .await
    .unwrap();

    let read = client
        .read_at(Address::repeat_byte(0x22), balance_of(), BlockId::number(5))
        .await;
    assert_ne!(
        read.ok(),
        Some(U256::from(AT_THE_HEAD)),
        "a read at block 5 was answered with the head's state"
    );
    assert!(
        ignoring.lock().unwrap().iter().all(Option::is_none),
        "a past read was sent to an endpoint that ignores the block and was never checked"
    );
}
