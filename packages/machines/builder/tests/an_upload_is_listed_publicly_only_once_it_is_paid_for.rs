//! An upload is listed in the public queue only once it is paid for. Until
//! then it is anyone's, naming any wallet with any title, and is asked for
//! by its id alone.
//!
//! Sequence: an author uploads a project. The public list is empty, and the
//! job asked for by its id is awaiting payment. The author pays, and the job
//! is listed.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_is_listed_publicly_only_once_it_is_paid_for() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0xa2);
    let (status, body) = builder.upload(author, &black_box::project("paid")).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    let listed = builder.listed().await;
    assert!(listed.is_empty(), "an unpaid upload is listed: {listed:?}");
    assert_eq!(
        builder.job(&accepted.job_id).await.map(|job| job.state),
        Some(JobState::AwaitingPayment)
    );

    let reference: B256 = accepted.upload.parse().unwrap();
    builder.node.pay(0x18, 5, author, reference);
    builder.node.head.store(0x20, Ordering::SeqCst);
    let started = Instant::now();
    while !builder
        .listed()
        .await
        .iter()
        .any(|job| job.id == accepted.job_id)
    {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "a paid upload is never listed"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
