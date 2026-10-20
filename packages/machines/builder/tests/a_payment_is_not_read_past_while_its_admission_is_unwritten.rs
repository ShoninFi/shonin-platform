//! Where payments were read to is not kept past a payment whose admission
//! could not be written down. Were it kept, a restart would find the job
//! still awaiting payment in `queue.json` and never read the payment again:
//! the $1 lost, the upload forgotten a day later, and no credit ever made.
//!
//! Sequence: `queue.json` holds upload 8800 waiting for payment against R.
//! `queue.json.new` can't be made (here a directory is in the way; in
//! production, a full disk). The author pays R at block 0x20, and the
//! builder reads that block again, failing each time to write the queue
//! down. It restarts on what it left in its store, the obstruction
//! gone: job 8800 is admitted on the payment.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_is_not_read_past_while_its_admission_is_unwritten() {
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
    let first = black_box::start(black_box::node(0x10, None), &[queue]).await;
    std::fs::create_dir_all(first.store().join("queue.json.new")).unwrap();
    let paid_at = 0x20;
    first.node.pay(paid_at, 5, author, reference);
    first.node.head.store(0x30, Ordering::SeqCst);

    // Wait until the block holding the payment has been read more than once
    // (a turn that failed is tried again a few seconds on).
    let started = Instant::now();
    while first
        .node
        .ranges
        .lock()
        .unwrap()
        .iter()
        .filter(|&&(from, to)| (from..=to).contains(&paid_at))
        .count()
        < 2
    {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the payment's block was never read"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut kept = vec![(
        "queue.json",
        std::fs::read_to_string(first.store().join("queue.json")).unwrap(),
    )];
    let payments = std::fs::read_to_string(first.store().join("payments.json")).ok();
    if let Some(payments) = &payments {
        kept.push(("payments.json", payments.clone()));
    }
    drop(first);

    let node = black_box::node(0x30, None);
    node.pay(paid_at, 5, author, reference);
    let second = black_box::start(node, &kept).await;
    let jobs = second
        .wait_for(Duration::from_secs(8), |jobs| {
            jobs.iter().any(|job| job.strategy_id == Some(5))
        })
        .await;
    assert!(
        jobs.iter()
            .any(|job| job.id == "8800" && job.state != JobState::AwaitingPayment),
        "after a restart with payments.json {payments:?}, the paid upload is still waiting \
         for its payment and its payment will never be read again: {jobs:?}"
    );
}
