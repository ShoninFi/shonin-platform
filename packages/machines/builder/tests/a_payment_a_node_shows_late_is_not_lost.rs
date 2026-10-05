//! A payment a node shows late is not lost. Payments are read only once
//! they are the chain's confirmations deep, and that many blocks behind
//! where the last look ended are read again on every look. A payment read
//! past while it was not there yet (a reorg brought it into a block that
//! replaced one already read, or a load-balanced provider's logs node was
//! behind the node that answered the head) would otherwise never be read:
//! the upload forgotten a day later, and the $1 lost.
//!
//! Sequence: the chain asks for 12 confirmations and stands at 0x30, so the
//! builder reads payments through 0x24. A payment for upload 8800 then
//! shows up at 0x1f, five blocks deeper than the confirmations, and the
//! head moves on by one. The job is admitted. (Read to the head, the builder
//! would have gone on from 0x31 and looked again only from 0x25; read
//! through 0x24 but never again, it would have gone on from 0x25.)

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_a_node_shows_late_is_not_lost() {
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
    let mut node = black_box::node(0x30, None);
    node.confirmations = Some(12);
    let builder = black_box::start(node, &[queue]).await;
    let started = Instant::now();
    while !builder
        .node
        .ranges
        .lock()
        .unwrap()
        .iter()
        .any(|&(_, to)| to >= 0x24)
    {
        assert!(started.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    builder.node.pay(0x1f, 5, author, reference);
    builder.node.head.store(0x31, Ordering::SeqCst);
    let jobs = builder
        .wait_for(Duration::from_secs(8), |jobs| {
            jobs.iter().any(|job| job.strategy_id == Some(5))
        })
        .await;
    assert!(
        jobs.iter()
            .any(|job| job.id == "8800" && job.state != JobState::AwaitingPayment),
        "a payment shown late in a block already read is never read: {jobs:?}"
    );
}

/// A payment at the head is not read until it is the chain's confirmations
/// deep.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_is_read_only_once_it_is_confirmed() {
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
    let mut node = black_box::node(0x30, None);
    node.confirmations = Some(12);
    node.pay(0x30, 5, author, reference);
    let builder = black_box::start(node, &[queue]).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let ranges = builder.node.ranges.lock().unwrap().clone();
    assert!(
        ranges.iter().all(|&(_, to)| to <= 0x24),
        "blocks past the confirmed head were read: {ranges:?}"
    );
    assert!(
        builder
            .jobs()
            .iter()
            .all(|job| job.state == JobState::AwaitingPayment),
        "a payment at the head was acted on"
    );

    builder.node.head.store(0x3c, Ordering::SeqCst);
    let jobs = builder
        .wait_for(Duration::from_secs(8), |jobs| {
            jobs.iter().any(|job| job.strategy_id == Some(5))
        })
        .await;
    assert!(
        jobs.iter().any(|job| job.strategy_id == Some(5)),
        "a payment 12 blocks deep was never read: {jobs:?}"
    );
}
