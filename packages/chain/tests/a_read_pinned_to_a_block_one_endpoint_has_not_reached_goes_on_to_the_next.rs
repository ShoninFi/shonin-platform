//! A read pinned to a block that one endpoint of the list has not reached
//! yet is answered by the next endpoint that has.
//!
//! A chain may name several endpoints, and they are never at the same block:
//! on a chain with fast blocks the one asked for a pinned read is often a
//! block behind the one that just gave the head. A service that reads the
//! head and then reads state pinned to that very block (the executor does,
//! for what it posts about a venue account: the pending balance and the
//! station's counters, all at one block) then hands the pinned read to an
//! endpoint that answers "header not found". That is the endpoint's own
//! lag, not the chain's answer about the call: as with a log window an
//! endpoint has not reached, the read goes on to the next endpoint rather
//! than failing, and the one that answered is asked first next time. When
//! no endpoint has the block the read is asked again a few times and then
//! fails; a revert stays the answer.
//!
//! Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::{Arc, Mutex};

use alloy::eips::BlockId;
use alloy::primitives::{Address, B256, U256};
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::{Value, json};
use stand_in::Said;

const HEAD: u64 = 1_000;

fn number(value: &Value) -> Option<u64> {
    u64::from_str_radix(value.as_str()?.strip_prefix("0x")?, 16).ok()
}

fn word(value: u64) -> Said {
    Said::Result(json!(B256::from(U256::from(value))))
}

fn refusal(request: &Value, message: &str, code: i64) -> Said {
    Said::Raw(
        200,
        json!({
            "jsonrpc": "2.0",
            "id": request["id"].clone(),
            "error": {"code": code, "message": message},
        })
        .to_string(),
    )
}

/// The blocks each contract read was asked at.
type Reads = Arc<Mutex<Vec<Option<u64>>>>;

/// An endpoint that has reached block `reached`, honours the block a read is
/// pinned to, and says so, as nodes do, when asked for one it does not have
/// yet.
fn endpoint(reached: u64, value: u64, reads: Reads) -> stand_in::Answer {
    Arc::new(move |method, request| {
        if let Some(said) = stand_in::past_block_check(method, request, stand_in::Past::Honours) {
            return said;
        }
        match method {
            "eth_blockNumber" => Said::Result(json!(format!("0x{reached:x}"))),
            "eth_call" => {
                let block = number(&request["params"][1]);
                reads.lock().unwrap().push(block);
                match block {
                    Some(block) if block > reached => refusal(request, "header not found", -32000),
                    _ => word(value),
                }
            }
            _ => stand_in::unknown_method(),
        }
    })
}

/// An endpoint that has every block and reverts every call.
fn reverting(reads: Reads) -> stand_in::Answer {
    Arc::new(move |method, request| {
        if let Some(said) = stand_in::past_block_check(method, request, stand_in::Past::Honours) {
            return said;
        }
        match method {
            "eth_blockNumber" => Said::Result(json!(format!("0x{HEAD:x}"))),
            "eth_call" => {
                reads.lock().unwrap().push(number(&request["params"][1]));
                refusal(request, "execution reverted: unknown block", 3)
            }
            _ => stand_in::unknown_method(),
        }
    })
}

fn balance_of() -> IERC20::balanceOfCall {
    IERC20::balanceOfCall {
        account: Address::ZERO,
    }
}

async fn client_over(urls: &[String]) -> chain::Client {
    chain::Client::connect(&stand_in::config(&urls.join(" ")), Signing::ReadOnly)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_pinned_to_the_head_one_endpoint_is_a_block_behind_is_answered_by_the_next() {
    let (behind, there): (Reads, Reads) = Default::default();
    let first = stand_in::answering(Some(31337), endpoint(HEAD - 1, 99, behind.clone())).await;
    let second = stand_in::answering(Some(31337), endpoint(HEAD, 7, there.clone())).await;
    let client = client_over(&[first, second]).await;

    let value = client
        .read_at(
            Address::repeat_byte(0x22),
            balance_of(),
            BlockId::number(HEAD),
        )
        .await;
    assert!(
        !behind.lock().unwrap().is_empty(),
        "the endpoint a block behind was never asked, so the test proves nothing"
    );
    match value {
        Ok(value) => assert_eq!(value, U256::from(7u64)),
        Err(error) => panic!(
            "an endpoint of the list has reached the block, and the read failed on the one that \
             had not: {error:#}"
        ),
    }
    assert_eq!(*there.lock().unwrap(), vec![Some(HEAD)]);

    // The endpoint that answered is asked first for the next pinned read:
    // the one behind is not asked again while the other answers.
    let asked_before = behind.lock().unwrap().len();
    for _ in 0..3 {
        let value = client
            .read_at(
                Address::repeat_byte(0x22),
                balance_of(),
                BlockId::number(HEAD),
            )
            .await
            .unwrap();
        assert_eq!(value, U256::from(7u64));
    }
    assert_eq!(behind.lock().unwrap().len(), asked_before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_pinned_to_a_block_no_endpoint_has_reached_fails_after_asking_again() {
    let (one, two): (Reads, Reads) = Default::default();
    let first = stand_in::answering(Some(31337), endpoint(HEAD - 2, 99, one.clone())).await;
    let second = stand_in::answering(Some(31337), endpoint(HEAD - 1, 7, two.clone())).await;
    let client = client_over(&[first, second]).await;

    let error = client
        .read_at(
            Address::repeat_byte(0x22),
            balance_of(),
            BlockId::number(HEAD),
        )
        .await
        .expect_err("no endpoint has the block");
    assert!(
        format!("{error:#}").contains("not reached the block"),
        "{error:#}"
    );
    // Each endpoint was asked more than once: the read was tried again.
    assert!(one.lock().unwrap().len() > 1, "{:?}", one.lock().unwrap());
    assert!(two.lock().unwrap().len() > 1, "{:?}", two.lock().unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revert_from_the_first_endpoint_is_the_answer_whatever_its_reason_says() {
    let (reverted, other): (Reads, Reads) = Default::default();
    let first = stand_in::answering(Some(31337), reverting(reverted.clone())).await;
    let second = stand_in::answering(Some(31337), endpoint(HEAD, 7, other.clone())).await;
    let client = client_over(&[first, second]).await;

    client
        .read_at(
            Address::repeat_byte(0x22),
            balance_of(),
            BlockId::number(HEAD),
        )
        .await
        .expect_err("a revert is the chain's answer");
    assert_eq!(*reverted.lock().unwrap(), vec![Some(HEAD)]);
    assert!(
        other.lock().unwrap().is_empty(),
        "a revert was taken for lag"
    );
}
