//! A payment is admitted on a chain whose node, like most paid providers,
//! caps how many blocks one look for logs may span: payments are read a page
//! at a time, at most 2,000 blocks, halving the page until the node answers.
//!
//! Sequence: the node refuses a look spanning more than 500 blocks, and the
//! publishing contract was deployed 20,000 blocks ago. An author uploads and
//! pays at block 20,010; the job is queued (and reviewed) soon after, and no
//! look ever spans more than 2,000 blocks.

mod black_box;

use std::time::Duration;

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_is_admitted_on_a_chain_whose_node_caps_log_ranges() {
    let head = 20_000;
    let builder = black_box::start(black_box::node(head, Some(500)), &[]).await;
    let author = Address::repeat_byte(0xa2);
    let (status, body) = builder.upload(author, &black_box::project("paid")).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    let reference: B256 = accepted.upload.parse().unwrap();

    // The payment lands, and the chain moves on past it.
    builder.node.pay(head + 10, 5, author, reference);
    builder
        .node
        .head
        .store(head + 20, std::sync::atomic::Ordering::SeqCst);

    let jobs = builder
        .wait_for(Duration::from_secs(15), |jobs| {
            jobs.iter()
                .any(|job| job.state != JobState::AwaitingPayment)
        })
        .await;
    let state = jobs
        .iter()
        .find(|job| job.id == accepted.job_id)
        .map(|job| job.state.clone());
    let ranges = builder.node.ranges.lock().unwrap().clone();
    assert!(
        !matches!(state, Some(JobState::AwaitingPayment) | None),
        "the job is still awaiting payment ({state:?}); the last looks asked for blocks {:?}",
        ranges.iter().rev().take(3).collect::<Vec<_>>()
    );
    let widest = ranges.iter().map(|(from, to)| to - from + 1).max();
    assert!(widest <= Some(2_000), "a look spanned {widest:?} blocks");
}
