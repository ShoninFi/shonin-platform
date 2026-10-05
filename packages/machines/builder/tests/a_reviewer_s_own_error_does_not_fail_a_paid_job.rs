//! Only a verdict ends a paid job. Any other answer from the reviewer is our
//! side's trouble, not the author's: its model's program could not be
//! started or gave no answer, its node refused the send that records a
//! refusal, its store could not be written. Whatever status it answers with
//! (an error of its own, not only 502, 503 or 504), the job goes back in the
//! queue, its $1 kept and the take not counted against it, and asks again
//! later.
//!
//! Here an author uploads a project and pays while every ask is answered
//! 500, then with a success whose body is not a verdict. The job waits in
//! the queue through both, and is reviewed once the reviewer answers again.

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
            "the reviewer was not asked:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Long enough for the builder to act on what it was answered.
    tokio::time::sleep(Duration::from_millis(500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reviewer_s_own_error_does_not_fail_a_paid_job() {
    black_box::ERRORS.store(true, Ordering::SeqCst);
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0x7a);
    let (status, body) = builder
        .upload(author, &black_box::project("model-down"))
        .await;
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

    asked(&builder, 1).await;
    assert!(
        matches!(job(&builder).state, JobState::Queued { .. }),
        "a paid job the reviewer answered with an error of its own is not back in the queue \
         with its payment kept: {:?}\n{}",
        job(&builder).state,
        builder.said.lock().unwrap()
    );
    assert_eq!(
        attempts(&builder),
        0,
        "a take the reviewer gave no verdict for was counted"
    );

    // Then answered with a success that is not a verdict.
    black_box::ERRORS.store(false, Ordering::SeqCst);
    black_box::GARBLED.store(true, Ordering::SeqCst);
    asked(&builder, 2).await;
    assert!(
        matches!(job(&builder).state, JobState::Queued { .. }),
        "a paid job the reviewer answered with something other than a verdict is not back \
         in the queue with its payment kept: {:?}\n{}",
        job(&builder).state,
        builder.said.lock().unwrap()
    );
    assert_eq!(
        attempts(&builder),
        0,
        "a take the reviewer gave no verdict for was counted"
    );

    // The reviewer answers again: the job is reviewed (and, the stand-in
    // refusing all it reads, refused).
    black_box::GARBLED.store(false, Ordering::SeqCst);
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
        "the job was not reviewed once the reviewer answered again:\n{}",
        builder.said.lock().unwrap()
    );
}
