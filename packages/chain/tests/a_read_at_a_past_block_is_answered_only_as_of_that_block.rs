//! A read pinned to a past block is answered only by an endpoint that
//! answers it as of that block.
//!
//! Some public endpoints answer a read at any block with the head's state,
//! and no error, so no wording of theirs can be caught. The record's hourly
//! readings and the executor's reads a few blocks deep would then be
//! today's values, shown as history. The client finds out at connect which
//! endpoints honour the block by asking each about state, as a pinned read
//! does: the nonce, as of the block before, of a sender whose transaction
//! is in a block below the head. One that answers the nonce the sender used
//! in that block honours it; one that answers more ignores it; one that says
//! it no longer keeps that state counts as honouring (a read moves on from
//! it); one that can't be asked is allowed. Pinned reads go only to those
//! not found to ignore the block. Reads at the head still go to every
//! endpoint.
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
use stand_in::{Past, Said};

const HEAD: u64 = 1_000;

fn number(value: &Value) -> Option<u64> {
    u64::from_str_radix(value.as_str()?.strip_prefix("0x")?, 16).ok()
}

fn word(value: u64) -> Said {
    Said::Result(json!(B256::from(U256::from(value))))
}

/// How a stand-in endpoint answers.
#[derive(Clone, Copy)]
struct Kind {
    /// How it answers a read of state at a past block, or `None` if it
    /// can't be asked (it knows none of the methods the check uses).
    past: Option<Past>,
    /// What a read of the contract returns.
    value: u64,
    /// Whether it limits the rate of every read at the head.
    limits_head_reads: bool,
}

/// The blocks each contract read was asked at (`None` for the head).
type Reads = Arc<Mutex<Vec<Option<u64>>>>;

fn endpoint(kind: Kind, reads: Reads) -> stand_in::Answer {
    Arc::new(move |method, request| {
        if let Some(past) = kind.past
            && let Some(said) = stand_in::past_block_check(method, request, past)
        {
            return said;
        }
        match method {
            "eth_blockNumber" => Said::Result(json!(format!("0x{HEAD:x}"))),
            "eth_call" => {
                let block = number(&request["params"][1]);
                reads.lock().unwrap().push(block);
                if block.is_none() && kind.limits_head_reads {
                    return Said::Raw(429, "Too Many Requests".to_string());
                }
                if block.is_some() && kind.past == Some(Past::Pruned) {
                    return Said::Raw(200, stand_in::missing_trie_node(request));
                }
                word(kind.value)
            }
            _ => stand_in::unknown_method(),
        }
    })
}

const IGNORES: Kind = Kind {
    past: Some(Past::Ignores),
    value: 99,
    limits_head_reads: false,
};
const HONOURS: Kind = Kind {
    past: Some(Past::Honours),
    value: 7,
    limits_head_reads: false,
};
const PRUNED: Kind = Kind {
    past: Some(Past::Pruned),
    value: 55,
    limits_head_reads: false,
};
const UNKNOWN: Kind = Kind {
    past: None,
    value: 7,
    limits_head_reads: false,
};

async fn client_over(kinds: &[(Kind, Reads)]) -> chain::Client {
    let mut urls = Vec::new();
    for (kind, reads) in kinds {
        urls.push(stand_in::answering(Some(31337), endpoint(*kind, reads.clone())).await);
    }
    chain::Client::connect(&stand_in::config(&urls.join(" ")), Signing::ReadOnly)
        .await
        .unwrap()
}

fn balance_of() -> IERC20::balanceOfCall {
    IERC20::balanceOfCall {
        account: Address::ZERO,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_past_read_never_lands_on_an_endpoint_that_ignores_the_block() {
    // Either way round in the list.
    for ignoring_first in [true, false] {
        let (ignoring, honest): (Reads, Reads) = Default::default();
        let mut kinds = vec![(IGNORES, ignoring.clone()), (HONOURS, honest.clone())];
        if !ignoring_first {
            kinds.reverse();
        }
        let client = client_over(&kinds).await;
        for _ in 0..5 {
            let value = client
                .read_at(Address::repeat_byte(0x22), balance_of(), BlockId::number(5))
                .await
                .unwrap();
            assert_eq!(value, U256::from(7u64), "answered with the head's state");
        }
        assert!(
            ignoring.lock().unwrap().is_empty(),
            "a past read was sent to the endpoint that ignores the block"
        );
        assert_eq!(*honest.lock().unwrap(), vec![Some(5); 5]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_only_endpoints_that_ignore_the_block_a_past_read_fails_naming_the_chain() {
    let (one, two): (Reads, Reads) = Default::default();
    let client = client_over(&[(IGNORES, one.clone()), (IGNORES, two.clone())]).await;
    let error = client
        .read_at(Address::repeat_byte(0x22), balance_of(), BlockId::number(5))
        .await
        .expect_err("no endpoint answers block 5 as of block 5");
    assert!(format!("{error:#}").contains("stand-in"), "{error:#}");
    assert!(one.lock().unwrap().is_empty() && two.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_endpoint_that_no_longer_keeps_the_checked_block_is_still_asked_and_passed_over() {
    // It counts as honouring the block: a past read is sent to it, and moves
    // on when it says it no longer keeps that block's state.
    let (pruned, honest): (Reads, Reads) = Default::default();
    let client = client_over(&[(PRUNED, pruned.clone()), (HONOURS, honest.clone())]).await;
    let value = client
        .read_at(Address::repeat_byte(0x22), balance_of(), BlockId::number(5))
        .await
        .unwrap();
    assert_eq!(value, U256::from(7u64));
    assert_eq!(*pruned.lock().unwrap(), vec![Some(5)]);
    assert_eq!(*honest.lock().unwrap(), vec![Some(5)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_endpoint_that_cannot_be_checked_is_still_asked_a_past_read() {
    let unknown: Reads = Default::default();
    let client = client_over(&[(UNKNOWN, unknown.clone())]).await;
    let value = client
        .read_at(Address::repeat_byte(0x22), balance_of(), BlockId::number(5))
        .await
        .unwrap();
    assert_eq!(value, U256::from(7u64));
    assert_eq!(*unknown.lock().unwrap(), vec![Some(5)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_at_the_head_still_uses_every_endpoint() {
    let (honest, ignoring): (Reads, Reads) = Default::default();
    // The endpoint that honours the block limits the rate of head reads, so
    // only the one that ignores the block can answer them.
    let limited = Kind {
        limits_head_reads: true,
        ..HONOURS
    };
    let client = client_over(&[(limited, honest.clone()), (IGNORES, ignoring.clone())]).await;
    let at_the_head = client
        .read(Address::repeat_byte(0x22), balance_of())
        .await
        .unwrap();
    assert_eq!(at_the_head, U256::from(99u64));
    let tagged = client
        .read_at(Address::repeat_byte(0x22), balance_of(), BlockId::latest())
        .await
        .unwrap();
    assert_eq!(tagged, U256::from(99u64));
}
