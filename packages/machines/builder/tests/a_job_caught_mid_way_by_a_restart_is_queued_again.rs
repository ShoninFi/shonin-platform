//! A job caught mid-way by a restart (a deploy, a crash, a preempted VM) is
//! queued again, and holds nothing of its author's up.
//!
//! The queue lets an author have one job in flight at a time, so a job left
//! `building` for good would hold every later job of theirs for good, each
//! paid for. The service stopping is not the author's fault, so the job is
//! taken again from the start.
//!
//! Sequence: `queue.json` holds job 8800 (author A) as `building` and job
//! 8801 (author A, paid for since) as `queued`. On the restart both are
//! reviewed, 8800 first.

mod black_box;

use std::time::Duration;

use alloy::primitives::Address;
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_caught_mid_way_by_a_restart_is_queued_again() {
    let author = Address::repeat_byte(0xa1);
    let queue = black_box::queue(vec![
        black_box::a_job(
            "8800",
            author,
            Some(1),
            JobState::Building,
            time::Duration::ZERO,
        ),
        black_box::a_job(
            "8801",
            author,
            Some(2),
            JobState::Queued { position: 0 },
            time::Duration::ZERO,
        ),
    ]);
    let builder = black_box::start(black_box::node(0x10, None), &[queue]).await;
    let jobs = builder
        .wait_for(Duration::from_secs(15), |jobs| {
            jobs.iter().all(|job| job.state == JobState::Refused)
        })
        .await;

    let reviewed = builder.reviewed.lock().unwrap().clone();
    let states: Vec<(String, &str)> = jobs
        .iter()
        .map(|job| (job.id.clone(), job.state.label()))
        .collect();
    assert_eq!(
        reviewed,
        [1, 2],
        "after the restart, the reviews asked for were {reviewed:?} and the queue reads \
         {states:?}: the job the restart interrupted holds its author's queue"
    );
}
