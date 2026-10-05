//! An upload paid for after it was forgotten is admitted, on that payment,
//! when its author sends it again: nobody pays twice because the service
//! forgot first.
//!
//! Sequence: `queue.json` holds an upload that arrived 25 hours ago, and it
//! is forgotten. Its author then pays for it. The same project sent again by
//! the same author is given the reference that was paid, and is queued and
//! reviewed under the paid id with no second payment. Sent once more, it is
//! an upload waiting for a payment of its own: a payment admits one job.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_paid_for_after_it_was_forgotten_is_admitted_when_sent_again() {
    let author = Address::repeat_byte(0x77);
    let reference = B256::repeat_byte(0x44);
    let archive = black_box::project("late");
    let queue = black_box::queue(vec![black_box::waiting(
        "8800",
        author,
        reference,
        &archive,
        time::Duration::hours(25),
    )]);
    let builder = black_box::start(black_box::node(0x10, None), &[queue]).await;
    let jobs = builder
        .wait_for(Duration::from_secs(10), |jobs| jobs.is_empty())
        .await;
    assert!(jobs.is_empty(), "the upload was never forgotten: {jobs:?}");

    builder.node.pay(0x20, 9, author, reference);
    builder.node.head.store(0x30, Ordering::SeqCst);
    let started = Instant::now();
    while builder.queue_file()["credits"]
        .as_array()
        .is_none_or(Vec::is_empty)
    {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the late payment was never credited:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let (status, body) = builder.upload(author, &archive).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    assert_eq!(accepted.upload.parse::<B256>().unwrap(), reference);
    let jobs = builder
        .wait_for(Duration::from_secs(10), |_| {
            builder.reviewed.lock().unwrap().contains(&9)
        })
        .await;
    let job = jobs.iter().find(|job| job.id == accepted.job_id).unwrap();
    assert_eq!(job.strategy_id, Some(9));
    assert_ne!(job.state, JobState::AwaitingPayment);
    assert_eq!(*builder.reviewed.lock().unwrap(), [9]);

    let (status, body) = builder.upload(author, &archive).await;
    assert_eq!(status, 200, "{body}");
    let again: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    assert_ne!(again.upload.parse::<B256>().unwrap(), reference);
    let job = builder
        .jobs()
        .into_iter()
        .find(|job| job.id == again.job_id)
        .unwrap();
    assert_eq!(job.state, JobState::AwaitingPayment);
}
