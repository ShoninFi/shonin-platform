//! A job that has ended keeps no archive, in `queue.json`, in the store or as
//! unpacked files: it is never built again, and a built job's source is in
//! the store.
//!
//! Sequence: `queue.json` holds one paid job, `queued`. The stand-in reviewer
//! refuses it. Once it is refused, its entry in `queue.json` has no archive
//! (and keeps the rest), no archive is kept for it in the store, and nothing
//! of it is left under the builds directory.

mod black_box;

use std::time::Duration;

use alloy::primitives::Address;
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_that_has_ended_keeps_no_archive() {
    let queue = black_box::queue(vec![black_box::a_job(
        "8800",
        Address::repeat_byte(0xa1),
        Some(1),
        JobState::Queued { position: 0 },
        time::Duration::ZERO,
    )]);
    let builder = black_box::start(black_box::node(0x10, None), &[queue]).await;
    let jobs = builder
        .wait_for(Duration::from_secs(10), |jobs| {
            jobs.iter().all(|job| job.state == JobState::Refused)
        })
        .await;
    assert_eq!(jobs[0].state, JobState::Refused);

    let kept = builder.queue_file();
    let entry = &kept["jobs"][0];
    assert_eq!(entry["job"]["id"], "8800", "the job's record is kept");
    assert!(
        entry.get("archive").is_none(),
        "queue.json still holds an archive"
    );
    let kept: Vec<_> = std::fs::read_dir(builder.store().join("archives"))
        .map(|found| found.flatten().map(|entry| entry.file_name()).collect())
        .unwrap_or_default();
    assert!(
        kept.is_empty(),
        "a job that has ended still keeps its archive: {kept:?}"
    );
    let left: Vec<_> = std::fs::read_dir(builder.builds())
        .map(|found| found.flatten().map(|entry| entry.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "a job that has ended left {left:?} behind");
}
