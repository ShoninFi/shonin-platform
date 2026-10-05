//! A `queue.json` cut short (a crash part way through writing it, a full
//! disk) stops the builder with a clear word, rather than being read as an
//! empty queue: that would lose every paid job and give their ids out again.
//! (The queue is now written whole or not at all, so the builder does not
//! cut its own file short.)
//!
//! Sequence: `queue.json` holds the first three quarters of a queue of two
//! paid jobs. The builder exits, failing, and names the file; it says to
//! restore it, or a backup, in step with `payments.json`, and never to start
//! empty.

mod black_box;

use std::time::{Duration, Instant};

use alloy::primitives::Address;
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queue_cut_short_by_a_crash_stops_the_builder() {
    let (name, whole) = black_box::queue(vec![
        black_box::a_job(
            "8800",
            Address::repeat_byte(0xa4),
            Some(1),
            JobState::Queued { position: 0 },
            time::Duration::ZERO,
        ),
        black_box::a_job(
            "8801",
            Address::repeat_byte(0xa5),
            Some(2),
            JobState::Queued { position: 0 },
            time::Duration::ZERO,
        ),
    ]);
    let cut = whole[..whole.len() * 3 / 4].to_string();
    let mut builder = black_box::spawn(black_box::node(0x10, None), &[(name, cut)]).await;

    let started = Instant::now();
    let status = loop {
        if let Some(status) = builder.child.try_wait().unwrap() {
            break status;
        }
        let open = reqwest::get(format!("{}/jobs", builder.url)).await;
        if let Ok(reply) = open {
            let listed = reply.text().await.unwrap_or_default();
            panic!("the builder opened on a queue cut short, listing {listed}");
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the builder neither opened nor stopped"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(!status.success());
    // Give the output a moment to be read.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let said = builder.said.lock().unwrap().clone();
    assert!(
        said.contains("queue.json"),
        "it stopped without naming the file: {said}"
    );
    assert!(
        said.contains("backup") && said.contains("payments.json") && !said.contains("empty"),
        "it did not say to restore the queue, in step with payments.json: {said}"
    );
}
