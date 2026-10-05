//! A review the reviewer gave its go-ahead for waits, on disk, for the build
//! to be registered, and is posted then. Each one is a paid job's: a strategy
//! registered with no review posted can never be activated, and the same
//! strategy can't be paid for and reviewed again. So the list of reviews
//! waiting to be posted is never lost quietly.
//!
//! It is written whole or not at all, and one that is there but can't be read
//! whole (a write cut short by a crash, a disk that filled, a copy restored
//! wrong) stops the reviewer, naming the file, rather than be read as
//! "nothing waiting".
//!
//! Here the store holds a `pending-reviews.json` that is not whole.

mod black_box;

use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reviews_waiting_to_be_posted_are_not_silently_dropped() {
    let mut reviewer = black_box::spawn(black_box::Options {
        node: true,
        store_files: vec![(
            "pending-reviews.json",
            r#"[{"chain":"base","strategy_id":5,"source_hash":"0x"#,
        )],
        ..Default::default()
    })
    .await;

    let started = Instant::now();
    let http = reqwest::Client::new();
    let stopped = loop {
        if let Some(status) = reviewer.child.try_wait().unwrap() {
            break Some(status);
        }
        assert!(
            http.get(format!("{}/health", reviewer.url))
                .send()
                .await
                .is_err(),
            "the reviewer opened with a list of waiting reviews it could not read, as if \
             nothing were waiting to be posted:\n{}",
            reviewer.said.lock().unwrap()
        );
        if started.elapsed() > Duration::from_secs(30) {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    // What it said on its way out is read to the end.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let said = reviewer.said.lock().unwrap().clone();
    assert!(
        stopped.is_some_and(|status| !status.success()),
        "the reviewer did not stop on a list of waiting reviews it could not read:\n{said}"
    );
    assert!(
        said.contains("pending-reviews.json"),
        "the reviewer stopped without naming the file it could not read:\n{said}"
    );
}
