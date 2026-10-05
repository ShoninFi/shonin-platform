//! `queue.json` holds no archive: each unfinished job's archive is a file of
//! its own in the store, so the queue written down on every change stays
//! small. A queue written before, archives and all, is moved out on the
//! first start.
//!
//! Sequence: `queue.json` holds paid job 8800 with its archive inside, as
//! queues once did. On the start, `queue.json` is written again without it,
//! the archive is in its own file, and the job is reviewed from it. Then an
//! upload is taken: `queue.json` names it without its archive, which is in a
//! file of its own, byte for byte as sent.

mod black_box;

use std::time::{Duration, Instant};

use alloy::primitives::Address;
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_queue_holds_no_archive() {
    let queue = black_box::queue(vec![black_box::a_job(
        "8800",
        Address::repeat_byte(0xa1),
        Some(1),
        JobState::Queued { position: 0 },
        time::Duration::ZERO,
    )]);
    assert!(queue.1.contains("\"archive\""));
    let builder = black_box::start(black_box::node(0x10, None), &[queue]).await;
    let started = Instant::now();
    while !builder.reviewed.lock().unwrap().contains(&1) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the job whose archive was moved out of the queue was never reviewed:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let author = Address::repeat_byte(0xa2);
    let archive = black_box::project("kept");
    let (status, body) = builder.upload(author, &archive).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    let written = std::fs::read_to_string(builder.store().join("queue.json")).unwrap();
    assert!(
        !written.contains("\"archive\""),
        "queue.json still holds an archive: {written}"
    );
    assert!(written.contains(&format!("\"{}\"", accepted.job_id)));
    let kept = std::fs::read(
        builder
            .store()
            .join("archives")
            .join(format!("{}.tar", accepted.job_id)),
    )
    .expect("the upload's archive is kept in a file of its own");
    assert_eq!(kept, archive);
}
