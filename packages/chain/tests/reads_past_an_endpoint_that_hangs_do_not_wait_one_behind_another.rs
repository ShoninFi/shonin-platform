//! Reads made at once, while the list's first endpoint has stopped
//! answering, each move on to the next endpoint after one request's
//! timeout. They do not wait one behind another.
//!
//! A list's endpoint is asked its chain id before its first request, and
//! one that doesn't answer is asked again on the next request. If the
//! requests waiting on that answer are let through one at a time, each
//! spending a whole request's timeout on an endpoint that hangs, the third
//! read made at the same moment moves on after three timeouts rather than
//! one, and a service that reads many things at once stalls for minutes on
//! an endpoint the list was meant to carry it past.
//!
//! Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use alloy::primitives::{Address, B256, U256};
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::json;
use stand_in::Said;

const HELD: u64 = 7;

/// An endpoint on the chain; once `hanging` is set it takes every request
/// and never answers.
fn endpoint(hanging: Arc<AtomicBool>) -> stand_in::Answer {
    Arc::new(move |method, _request| {
        if hanging.load(Ordering::SeqCst) {
            return Said::Silence;
        }
        match method {
            "eth_chainId" => Said::Result(json!("0x7a69")),
            "eth_blockNumber" => Said::Result(json!("0x10")),
            "eth_call" => Said::Result(json!(B256::from(U256::from(HELD)))),
            _ => stand_in::unknown_method(),
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_reads_made_at_once_all_move_on_after_one_timeout() {
    let first_hangs = Arc::new(AtomicBool::new(false));
    let first = stand_in::answering(None, endpoint(first_hangs.clone())).await;
    let second = stand_in::answering(None, endpoint(Arc::new(AtomicBool::new(false)))).await;
    let client = Arc::new(
        chain::Client::connect(
            &stand_in::config(&format!("{first} {second}")),
            Signing::ReadOnly,
        )
        .await
        .expect("both endpoints answer when the client connects"),
    );

    // The first endpoint stops answering before the client's first read.
    first_hangs.store(true, Ordering::SeqCst);

    let started = std::time::Instant::now();
    let mut reads = tokio::task::JoinSet::new();
    for _ in 0..3 {
        let client = client.clone();
        reads.spawn(async move {
            client
                .read(
                    Address::repeat_byte(0x22),
                    IERC20::balanceOfCall {
                        account: Address::ZERO,
                    },
                )
                .await
                .map(|held| (held, started.elapsed()))
        });
    }
    // A request is given 30 seconds; half as much again is room enough for
    // every read to give the first endpoint up once and ask the second.
    let within = Duration::from_secs(45);
    let mut answered = Vec::new();
    while let Ok(Some(read)) = tokio::time::timeout_at(
        tokio::time::Instant::from_std(started + within),
        reads.join_next(),
    )
    .await
    {
        let (held, after) = read.unwrap().expect("the second endpoint answers");
        assert_eq!(held, U256::from(HELD));
        answered.push(after);
    }
    assert_eq!(
        answered.len(),
        3,
        "of three reads made at once, only {} had moved on to the second endpoint {} seconds \
         after the first stopped answering (answered after {answered:?}): the rest were still \
         waiting their turn at the endpoint that hangs",
        answered.len(),
        within.as_secs(),
    );
}
