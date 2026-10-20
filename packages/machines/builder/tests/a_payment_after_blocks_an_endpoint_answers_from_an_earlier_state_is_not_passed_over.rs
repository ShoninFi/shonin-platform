//! A payment after blocks an endpoint answers from an earlier state is not
//! passed over. Blocks are passed over unread only on the registry's count
//! as of the very block asked, read through an endpoint known to answer so:
//! an endpoint that can't be checked, and answers every read from a state
//! behind its head, would show no payment where there is one. Such counts
//! are not taken, and the blocks are read page by page.
//!
//! Sequence: the chain stands at block 100,000, payments read from 0, and
//! its one endpoint can't be checked. An author paid at block 99,000 for an
//! upload waiting since an hour ago, and the endpoint answers the registry's
//! count and records as of block 90,000, whatever block is asked. The job
//! is admitted all the same.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::Duration;

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_after_blocks_an_endpoint_answers_from_an_earlier_state_is_not_passed_over() {
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
    let node = black_box::node(100_000, None);
    node.honours_past_blocks.store(false, Ordering::SeqCst);
    node.pay(99_000, 1, author, reference);
    node.stale_state.store(90_000, Ordering::SeqCst);
    let builder = black_box::start(node, &[queue]).await;
    let jobs = builder
        .wait_for(Duration::from_secs(60), |jobs| {
            jobs.iter()
                .any(|job| job.id == "8800" && job.state != JobState::AwaitingPayment)
        })
        .await;
    assert!(
        jobs.iter()
            .any(|job| job.id == "8800" && job.state != JobState::AwaitingPayment),
        "the payment was passed over: {jobs:?}\n{}",
        builder.said.lock().unwrap()
    );
}
