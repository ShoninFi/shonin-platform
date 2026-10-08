//! The reviewer runs on demand: woken by the build service when there is a
//! review to make, it exits once it has had nothing to do for a while, and
//! the machine powers off. Here it is told to exit after one idle second,
//! and has nothing to do: it stops listening and exits, cleanly, writing
//! down the reviews waiting to be posted (none) as it goes.

mod black_box;

use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reviewer_exits_cleanly_once_idle() {
    let mut reviewer = black_box::spawn(black_box::Options {
        node: true,
        args: vec!["--idle-exit-seconds", "1"],
        ..Default::default()
    })
    .await;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = reviewer.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the reviewer never exited once idle:\n{}",
            reviewer.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(
        status.success(),
        "the reviewer exited, but not cleanly ({status}):\n{}",
        reviewer.said.lock().unwrap()
    );
    assert!(
        reviewer
            .wait_until_said("idle: the reviewer exits", Duration::from_secs(5))
            .await,
        "{}",
        reviewer.said.lock().unwrap()
    );
    assert!(
        reviewer.store().join("pending-reviews.json").is_file(),
        "what waits to be posted was not written down on the way out"
    );
}
