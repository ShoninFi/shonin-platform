//! What is remembered of forgotten uploads, and the payments credited to
//! them, survive a restart: a payment that comes late for an upload
//! forgotten before the restart is still credited, and one credited before
//! it still admits the upload sent again. A payment read again after the
//! restart is not credited twice.
//!
//! Sequence: a first run forgets two uploads, then reads a payment for the
//! second, and is stopped. A second run, on the same `queue.json` but reading
//! payments again from the start, reads a payment for the first. Each project
//! sent again is queued under its own paid id, and no credit is left over.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

/// Wait until `queue.json` holds `count` credits.
async fn credits(builder: &black_box::Builder, count: usize) {
    let started = Instant::now();
    loop {
        let held = builder.queue_file()["credits"]
            .as_array()
            .map_or(0, Vec::len);
        if held == count {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "queue.json holds {held} credits, not {count}:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forgotten_uploads_and_their_credits_survive_a_restart() {
    let author = Address::repeat_byte(0x77);
    let (first_reference, second_reference) = (B256::repeat_byte(0x44), B256::repeat_byte(0x45));
    let (first_archive, second_archive) =
        (black_box::project("first"), black_box::project("second"));
    let queue = black_box::queue(vec![
        black_box::waiting(
            "8800",
            author,
            first_reference,
            &first_archive,
            time::Duration::hours(25),
        ),
        black_box::waiting(
            "8801",
            author,
            second_reference,
            &second_archive,
            time::Duration::hours(26),
        ),
    ]);
    let first = black_box::start(black_box::node(0x10, None), &[queue]).await;
    let jobs = first
        .wait_for(Duration::from_secs(10), |jobs| jobs.is_empty())
        .await;
    assert!(
        jobs.is_empty(),
        "the uploads were never forgotten: {jobs:?}"
    );
    first.node.pay(0x20, 5, author, second_reference);
    first.node.head.store(0x30, Ordering::SeqCst);
    credits(&first, 1).await;
    let kept = std::fs::read_to_string(first.store().join("queue.json")).unwrap();
    drop(first);

    let node = black_box::node(0x30, None);
    node.pay(0x20, 5, author, second_reference);
    node.pay(0x28, 6, author, first_reference);
    let second = black_box::start(node, &[("queue.json", kept)]).await;
    credits(&second, 2).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    credits(&second, 2).await;

    for (archive, reference, strategy_id) in [
        (&second_archive, second_reference, 5),
        (&first_archive, first_reference, 6),
    ] {
        let (status, body) = second.upload(author, archive).await;
        assert_eq!(status, 200, "{body}");
        let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
        assert_eq!(accepted.upload.parse::<B256>().unwrap(), reference);
        let job = second
            .jobs()
            .into_iter()
            .find(|job| job.id == accepted.job_id)
            .unwrap();
        assert_eq!(job.strategy_id, Some(strategy_id));
        assert_ne!(job.state, JobState::AwaitingPayment);
    }
    credits(&second, 0).await;
}
