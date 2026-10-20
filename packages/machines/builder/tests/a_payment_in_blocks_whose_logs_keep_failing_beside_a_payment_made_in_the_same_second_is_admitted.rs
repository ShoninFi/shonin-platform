//! A payment in blocks whose logs keep failing is admitted from the
//! registry's record even when the next payment was made in the same second
//! as the page's last block. The record proves which payments a page holds
//! by the times the payments on either side of it were made; two blocks of
//! the same time can't tell those apart, so the page is ended a block or so
//! earlier, where they can.
//!
//! Sequence: the chain stands at block 3,000. An author pays at 0x20 for an
//! upload waiting since an hour ago, and no log of block 0x20 can be read.
//! Another payment is made at block 0x20 + 2,000, the first block past the
//! page read from 0x20, in the same second as the block before it. The job is
//! admitted.

mod black_box;

use std::time::Duration;

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_in_blocks_whose_logs_keep_failing_beside_a_payment_made_in_the_same_second_is_admitted()
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
    let node = black_box::node(3_000, None);
    node.pay(0x20, 1, author, reference);
    node.pay(
        0x20 + 2_000,
        2,
        Address::repeat_byte(0x78),
        B256::repeat_byte(0x45),
    );
    node.same_time.lock().unwrap().push(0x20 + 2_000);
    node.unreadable.lock().unwrap().push(0x20);
    let builder = black_box::start(node, &[queue]).await;
    let jobs = builder
        .wait_for(Duration::from_secs(40), |jobs| {
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
}
