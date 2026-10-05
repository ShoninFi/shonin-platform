//! A payment that becomes a credit after the author has already sent the
//! same project again admits that upload at once, on the payment: the author
//! does not pay twice. A payment that arrives against a forgotten
//! upload becomes a credit, and the same author sending the same project
//! again is admitted under the paid id, with no second payment; that holds
//! whichever comes first, the payment being read or the project sent again.
//!
//! Sequence: the author uploads P (reference R1) and pays R1 at once. Before
//! the payment is read, three uploads naming the author's wallet push P out
//! under the per-wallet bound; P is forgotten to a note. The author sends P
//! again: there is no credit yet, so it is a new upload (reference R2)
//! waiting for payment. The payment for R1 is then read, and the upload sent
//! again is queued on it, leaving no credit behind.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::Duration;

use alloy::primitives::{Address, B256};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_read_after_the_same_upload_was_sent_again_admits_it() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0x77);
    let project = black_box::project("the author's agent");

    let (status, body) = builder.upload(author, &project).await;
    assert_eq!(status, 200, "{body}");
    let first: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    let r1: B256 = first.upload.parse().unwrap();

    // Three more uploads naming the same wallet push the first out.
    for n in 0..3 {
        let (status, body) = builder
            .upload(author, &black_box::project(&format!("other {n}")))
            .await;
        assert_eq!(status, 200, "{body}");
    }
    assert!(
        builder.job(&first.job_id).await.is_none(),
        "the first upload was not forgotten, so this test does not show anything"
    );

    // The author sends the same project again before the payment is read.
    let (status, body) = builder.upload(author, &project).await;
    assert_eq!(status, 200, "{body}");
    let again: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    assert_ne!(again.job_id, first.job_id);

    // Now the payment for the first reference is read.
    builder.node.pay(0x11, 5, author, r1);
    builder.node.head.store(0x12, Ordering::SeqCst);

    let jobs = builder
        .wait_for(Duration::from_secs(8), |jobs| {
            jobs.iter().any(|job| job.strategy_id == Some(5))
        })
        .await;
    let credits = builder.queue_file()["credits"].clone();
    let admitted = jobs
        .iter()
        .find(|job| job.strategy_id == Some(5))
        .map(|job| job.id.clone());
    assert_eq!(
        admitted,
        Some(again.job_id.clone()),
        "the author paid for this project and sent it again, yet upload {} still waits for a \
         second payment; the payment sits as a credit instead: {credits}",
        again.job_id
    );
    assert_eq!(credits, serde_json::json!([]), "the payment was used twice");
}
