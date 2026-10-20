//! A chain far behind is read to its head without its every block being
//! asked for. The build service is started only now and then, and a chain
//! with blocks a second apart is days of blocks behind by then: read a page
//! at a time through public endpoints that limit the rate, it would never
//! catch up, and would keep the machine up waiting on a payment it has not
//! got to. The registry's count of payments at past blocks says where the
//! next payment is, and the times its payments were made confirm that the
//! blocks before it hold none: those are passed over unread.
//!
//! Sequence: the chain stands at block 1,000,000, payments read from 0. An
//! author paid at block 990,000 for an upload waiting since an hour ago. The
//! job is admitted, and the logs asked for span a few pages, not a million
//! blocks. With no payment at all, the chain is read to its head the same
//! way.

mod black_box;

use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

/// How many blocks the builder asked the node's logs for, in all.
fn blocks_asked(builder: &black_box::Builder) -> u64 {
    builder
        .node
        .ranges
        .lock()
        .unwrap()
        .iter()
        .map(|(from, to)| to - from + 1)
        .sum()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_far_behind_is_admitted_without_reading_every_block_before_it() {
    let author = Address::repeat_byte(0x77);
    let reference = B256::repeat_byte(0x44);
    let archive = black_box::project("paid");
    let queue = black_box::queue(vec![black_box::waiting(
        "8800",
        author,
        reference,
        &archive,
        time::Duration::hours(1),
    )]);
    let node = black_box::node(1_000_000, None);
    node.pay(990_000, 1, author, reference);
    let builder = black_box::start(node, &[queue]).await;
    let jobs = builder
        .wait_for(Duration::from_secs(30), |jobs| {
            jobs.iter()
                .any(|job| job.id == "8800" && job.state != JobState::AwaitingPayment)
        })
        .await;
    assert!(
        jobs.iter()
            .any(|job| job.id == "8800" && job.state != JobState::AwaitingPayment),
        "the payment was never admitted: {jobs:?}\n{}",
        builder.said.lock().unwrap()
    );
    let asked = blocks_asked(&builder);
    assert!(
        asked <= 20_000,
        "{asked} blocks' logs were asked for to reach a payment a million blocks on"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_far_behind_with_no_payment_is_read_to_its_head_without_its_logs() {
    let head = 1_000_000;
    let builder = black_box::start(black_box::node(head, None), &[]).await;
    let started = Instant::now();
    let read_to = loop {
        let read: Option<serde_json::Value> = std::fs::read(builder.store().join("payments.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let at = read
            .as_ref()
            .and_then(|read| read["base"].as_u64())
            .unwrap_or(0);
        if at > head || started.elapsed() > Duration::from_secs(20) {
            break at;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(
        read_to > head,
        "payments were read only to block {read_to} of {head}:\n{}",
        builder.said.lock().unwrap()
    );
    let asked = blocks_asked(&builder);
    assert!(
        asked <= 20_000,
        "{asked} blocks' logs were asked for to read a chain with no payment"
    );
}
