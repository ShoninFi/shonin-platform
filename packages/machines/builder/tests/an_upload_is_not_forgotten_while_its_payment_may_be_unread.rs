//! An upload is not forgotten while its payment may be among the payments
//! not yet read, however long the clock says it has waited: the service
//! halts rather than lose a payment to its own timing.
//!
//! Sequence: `queue.json` holds an upload that arrived 25 hours ago, and the
//! node fails every look for payments. A few seconds on, the upload is still
//! listed and kept. Its author's payment lands, and the node recovers: the
//! payment is read, and the upload is admitted, not forgotten.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::Duration;

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_is_not_forgotten_while_its_payment_may_be_unread() {
    let author = Address::repeat_byte(0x77);
    let reference = B256::repeat_byte(0x44);
    let queue = black_box::queue(vec![black_box::waiting(
        "8800",
        author,
        reference,
        &black_box::project("late"),
        time::Duration::hours(25),
    )]);
    let node = black_box::node(0x10, None);
    node.logs_fail.store(true, Ordering::SeqCst);
    let builder = black_box::start(node, &[queue]).await;

    tokio::time::sleep(Duration::from_secs(4)).await;
    let jobs = builder.jobs();
    assert_eq!(
        jobs.iter().map(|job| &*job.id).collect::<Vec<_>>(),
        ["8800"],
        "an upload was forgotten before its payments were read"
    );
    assert_eq!(
        builder.queue_file()["jobs"].as_array().unwrap().len(),
        1,
        "an upload was forgotten before its payments were read"
    );

    builder.node.pay(0x08, 11, author, reference);
    builder.node.logs_fail.store(false, Ordering::SeqCst);
    let jobs = builder
        .wait_for(Duration::from_secs(10), |jobs| {
            jobs.iter()
                .any(|job| job.state != JobState::AwaitingPayment)
        })
        .await;
    let job = jobs.iter().find(|job| job.id == "8800");
    assert!(
        job.is_some_and(|job| job.strategy_id == Some(11) && job.state != JobState::AwaitingPayment),
        "the upload was not admitted on its payment: {jobs:?}\n{}",
        builder.said.lock().unwrap()
    );
}
