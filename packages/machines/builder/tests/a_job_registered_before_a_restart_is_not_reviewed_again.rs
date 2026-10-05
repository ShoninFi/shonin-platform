//! A job a restart caught after its registration landed is ready, with the
//! bundle the registry holds: it is not reviewed or built again. Taken again
//! from the start, the reviewer (which need not answer the same way twice)
//! could refuse it, or the registry refuse it as registered already, and
//! the author who paid would see a job that did not end ready for a
//! strategy that is ready to activate.
//!
//! Sequence: `queue.json` holds job 8800, strategy 5, as `building`; the
//! registry already holds strategy 5, and the store its bundle's files. On
//! the restart the job is ready, with the registered bundle, and the reviewer
//! is never asked about it.

mod black_box;

use std::time::Duration;

use alloy::primitives::Address;
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_registered_before_a_restart_is_not_reviewed_again() {
    let author = Address::repeat_byte(0xa1);
    let queue = black_box::queue(vec![black_box::a_job(
        "8800",
        author,
        Some(5),
        JobState::Building,
        time::Duration::ZERO,
    )]);
    let node = black_box::node(0x10, None);
    node.registered.lock().unwrap().push(5);
    let mut files = black_box::stored(black_box::REGISTERED_BUNDLE);
    files.push(queue);
    let builder = black_box::start(node, &files).await;
    let jobs = builder
        .wait_for(Duration::from_secs(8), |jobs| {
            jobs.iter().all(|job| job.state.is_finished())
        })
        .await;
    let reviewed = builder.reviewed.lock().unwrap().clone();
    assert!(
        reviewed.is_empty() && jobs.iter().all(|job| job.state == JobState::Ready),
        "strategy 5 is registered, yet after the restart it was reviewed again ({reviewed:?}) \
         and the job reads {:?}",
        jobs.iter().map(|job| job.state.label()).collect::<Vec<_>>()
    );
    assert_eq!(
        jobs[0].bundle_hash,
        Some(format!(
            "sha256:{}",
            alloy::hex::encode(black_box::REGISTERED_BUNDLE)
        ))
    );
}
