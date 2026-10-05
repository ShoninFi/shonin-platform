//! A second payment for an upload already paid for is kept as a credit, as
//! one for an upload forgotten is: the same upload sent again by the same
//! author is admitted on it. Nobody's $1 admits nothing.
//!
//! Sequence: an author uploads a project and pays for it twice, as strategy
//! 5 and then 6. The upload is admitted as 5, and 6 is kept as a credit,
//! once however often the payments are read. The same project sent again is
//! admitted as 6, against the same reference, with no third payment.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_payment_for_an_upload_is_kept_as_a_credit() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0xa2);
    let archive = black_box::project("paid twice");
    let (status, body) = builder.upload(author, &archive).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    let reference: B256 = accepted.upload.parse().unwrap();
    builder.node.pay(0x18, 5, author, reference);
    builder.node.pay(0x19, 6, author, reference);
    builder.node.head.store(0x20, Ordering::SeqCst);

    let started = Instant::now();
    while builder.queue_file()["credits"]
        .as_array()
        .is_none_or(Vec::is_empty)
    {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the second payment was not credited:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let credits = builder.queue_file()["credits"].clone();
    assert_eq!(credits.as_array().map(Vec::len), Some(1), "{credits}");
    assert_eq!(credits[0]["strategy_id"], 6);
    let first = builder
        .jobs()
        .into_iter()
        .find(|job| job.id == accepted.job_id)
        .unwrap();
    assert_eq!(first.strategy_id, Some(5));

    let (status, body) = builder.upload(author, &archive).await;
    assert_eq!(status, 200, "{body}");
    let again: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    assert_eq!(again.upload.parse::<B256>().unwrap(), reference);
    let job = builder
        .jobs()
        .into_iter()
        .find(|job| job.id == again.job_id)
        .unwrap();
    assert_eq!(job.strategy_id, Some(6));
    assert_ne!(job.state, JobState::AwaitingPayment);
}
