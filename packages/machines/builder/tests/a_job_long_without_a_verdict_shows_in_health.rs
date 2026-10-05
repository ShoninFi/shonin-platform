//! A paid job the reviewer gives no verdict for goes back in the queue and
//! asks again, waiting longer each time, for as long as it takes: it never
//! fails for our side's trouble. A reviewer that keeps not answering is
//! something to look into, so once a job has gone back in the queue more
//! than three times in a row the build service's health answers 503 and
//! names it, while the job still waits.
//!
//! Here the queue holds a paid job that has gone back three times in a row
//! already, and the reviewer answers with an error again.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::Duration;

use alloy::primitives::Address;
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_long_without_a_verdict_shows_in_health() {
    black_box::ERRORS.store(true, Ordering::SeqCst);
    let mut job = black_box::a_job(
        "8800",
        Address::repeat_byte(0xa2),
        Some(1),
        JobState::Queued { position: 0 },
        time::Duration::ZERO,
    );
    job["reviewer_away"] = 3.into();
    let builder =
        black_box::start(black_box::node(0x10, None), &[black_box::queue(vec![job])]).await;

    let http = reqwest::Client::new();
    let started = std::time::Instant::now();
    let (status, said) = loop {
        let reply = http
            .get(format!("{}/health", builder.url))
            .send()
            .await
            .unwrap();
        let status = reply.status().as_u16();
        let said = reply.text().await.unwrap();
        if status != 200 || started.elapsed() > Duration::from_secs(30) {
            break (status, said);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(
        black_box::ASKED.load(Ordering::SeqCst) >= 1,
        "the reviewer was never asked:\n{}",
        builder.said.lock().unwrap()
    );
    assert_eq!(
        status,
        503,
        "a job asked of the reviewer a fourth time in a row without a verdict does not show \
         in health: {said}\n{}",
        builder.said.lock().unwrap()
    );
    assert!(
        said.contains("8800"),
        "health does not name the job: {said}"
    );
    let state = builder
        .jobs()
        .into_iter()
        .find(|job| job.id == "8800")
        .expect("the job is listed")
        .state;
    assert!(
        matches!(state, JobState::Queued { .. }),
        "the job no longer waits in the queue: {state:?}"
    );
}
