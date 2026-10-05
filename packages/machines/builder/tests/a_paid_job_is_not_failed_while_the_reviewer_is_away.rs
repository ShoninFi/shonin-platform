//! A job caught mid-review or mid-build by a restart of ours goes back in
//! the queue: that is our failure, not a failed build. So does a job
//! whose review the reviewer does not answer: a reviewer restarting, being
//! deployed, or not yet connected to the job's chain (which it answers 503
//! for) is our side's trouble, not the author's. The job goes back in the
//! queue, its $1 kept and the take not counted against it, and asks again,
//! waiting longer each time, while the queue moves on.
//!
//! Here an author uploads a project and pays while the reviewer is away: it
//! first drops the request unanswered, then answers 503. The job waits in the
//! queue through both, and is reviewed once the reviewer is back.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::Duration;

use alloy::primitives::Address;
use shared::model::registry::JobState;

/// Wait until the stand-in reviewer has been asked `times` times in all.
async fn asked(builder: &black_box::Builder, times: u64) {
    let started = std::time::Instant::now();
    while black_box::ASKED.load(Ordering::SeqCst) < times {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the reviewer was not asked again:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Long enough for the builder to act on what it was answered.
    tokio::time::sleep(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paid_job_is_not_failed_while_the_reviewer_is_away() {
    black_box::AWAY.store(true, Ordering::SeqCst);
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0x79);
    let (status, body) = builder.upload(author, &black_box::project("away")).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    builder
        .node
        .pay(0x11, 5, author, accepted.upload.parse().unwrap());
    builder.node.head.store(0x12, Ordering::SeqCst);

    let job = |builder: &black_box::Builder| {
        builder
            .jobs()
            .into_iter()
            .find(|job| job.id == accepted.job_id)
            .expect("the upload is a job")
    };
    let attempts = |builder: &black_box::Builder| {
        builder.queue_file()["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["job"]["id"] == accepted.job_id.as_str())
            .map(|entry| entry["attempts"].as_u64().unwrap_or(0))
            .unwrap()
    };

    // The request is dropped unanswered.
    asked(&builder, 1).await;
    assert!(
        matches!(job(&builder).state, JobState::Queued { .. }),
        "a paid job whose review went unanswered is not back in the queue: {:?}",
        job(&builder).state
    );
    assert_eq!(
        attempts(&builder),
        0,
        "a take the reviewer did not answer was counted"
    );

    // Then answered 503: not connected to the job's chain yet.
    black_box::AWAY.store(false, Ordering::SeqCst);
    black_box::UNAVAILABLE.store(true, Ordering::SeqCst);
    asked(&builder, 2).await;
    assert!(
        matches!(job(&builder).state, JobState::Queued { .. }),
        "a paid job the reviewer answered 503 for is not back in the queue: {:?}",
        job(&builder).state
    );
    assert_eq!(
        attempts(&builder),
        0,
        "a take the reviewer answered 503 for was counted"
    );

    // Back: the job is reviewed (and, the stand-in refusing all it reads,
    // refused).
    black_box::UNAVAILABLE.store(false, Ordering::SeqCst);
    let jobs = builder
        .wait_for(Duration::from_secs(90), |jobs| {
            jobs.iter()
                .any(|job| job.id == accepted.job_id && job.state.is_finished())
        })
        .await;
    let ended = jobs
        .iter()
        .find(|job| job.id == accepted.job_id)
        .expect("the upload is a job");
    assert_eq!(
        ended.state,
        JobState::Refused,
        "the job was not reviewed once the reviewer was back:\n{}",
        builder.said.lock().unwrap()
    );
    assert_eq!(*builder.reviewed.lock().unwrap(), [5]);
}
