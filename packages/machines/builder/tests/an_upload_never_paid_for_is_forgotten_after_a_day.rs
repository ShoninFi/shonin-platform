//! An upload never paid for is forgotten, archive and all, a day after it
//! arrived, once the payments read reach that far, and is no longer listed on
//! the public queue. A note of it, without the archive, is kept.
//!
//! An upload can name any wallet, and nothing about it is paid for, so
//! nothing may keep it for good.
//!
//! Sequence: `queue.json` holds two uploads waiting for their payment, one
//! that arrived 25 hours ago and one that arrived a minute ago, and the
//! chain's blocks are stamped now. Soon after the start, the queue lists only
//! the second, `queue.json` holds only the second, and notes the first as
//! forgotten.

mod black_box;

use std::time::Duration;

use alloy::primitives::Address;
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_never_paid_for_is_forgotten_after_a_day() {
    let author = Address::repeat_byte(0x77);
    let queue = black_box::queue(vec![
        black_box::a_job(
            "8800",
            author,
            None,
            JobState::AwaitingPayment,
            time::Duration::hours(25),
        ),
        black_box::a_job(
            "8801",
            author,
            None,
            JobState::AwaitingPayment,
            time::Duration::minutes(1),
        ),
    ]);
    let builder = black_box::start(black_box::node(0x10, None), &[queue]).await;
    let jobs = builder
        .wait_for(Duration::from_secs(10), |jobs| jobs.len() == 1)
        .await;
    let listed: Vec<&str> = jobs.iter().map(|job| &*job.id).collect();
    assert_eq!(
        listed,
        ["8801"],
        "an upload unpaid for a day is still listed"
    );

    let written = builder.queue_file();
    let kept: Vec<String> = written["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["job"]["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kept, ["8801"], "an upload unpaid for a day is still kept");
    let forgotten = written["forgotten"].as_array().unwrap();
    assert_eq!(forgotten.len(), 1, "{written}");
    assert!(
        forgotten[0].get("archive").is_none(),
        "a forgotten upload's archive is kept"
    );

    // Ids are never given twice: the next upload is not 8800 or 8801 again.
    let (status, body) = builder
        .upload(Address::repeat_byte(0x78), &black_box::project("next"))
        .await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    assert_eq!(accepted.job_id, "8802");
}
