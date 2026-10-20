//! A payment far behind, made in the same second as the block before it,
//! is still reached without every block before it being read. The blocks
//! before the next payment are passed over when the times payments were
//! made prove they hold none; two blocks of the same time can't tell a
//! payment in one from one in the other, so the blocks passed over end a
//! block or so earlier, and only those few are read.
//!
//! Sequence: the chain stands at block 1,000,000, payments read from 0. An
//! author paid at block 990,000, made in the same second as block 989,999,
//! for an upload waiting since an hour ago. The job is admitted, and the
//! logs asked for span a few pages, not a million blocks.

mod black_box;

use std::time::Duration;

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_far_behind_made_in_the_same_second_as_the_block_before_is_reached_without_reading_every_block()
 {
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
    node.same_time.lock().unwrap().push(990_000);
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
    let asked: u64 = builder
        .node
        .ranges
        .lock()
        .unwrap()
        .iter()
        .map(|(from, to)| to - from + 1)
        .sum();
    assert!(
        asked <= 20_000,
        "{asked} blocks' logs were asked for to reach a payment a million blocks on"
    );
}
