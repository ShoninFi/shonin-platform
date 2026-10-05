//! A restart reads payments on from where the last run left off, not again
//! from the block the publishing contract was deployed at: where it has read
//! to is kept as each page is read.
//!
//! Sequence: a first run reads a chain 9,000 blocks long, and is stopped. A
//! second run, on the same store, never asks for a block the first one read.

mod black_box;

use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_reads_payments_on_from_where_it_left_off() {
    let head = 9_000;
    let first = black_box::start(black_box::node(head, None), &[]).await;
    let started = Instant::now();
    while !first
        .node
        .ranges
        .lock()
        .unwrap()
        .iter()
        .any(|&(_, to)| to >= head)
    {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the first run never read to the head"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let kept = std::fs::read_to_string(first.store().join("payments.json"))
        .expect("where payments were read to is kept");
    drop(first);

    let second = black_box::start(
        black_box::node(head, None),
        &[("payments.json", kept.clone())],
    )
    .await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let ranges = second.node.ranges.lock().unwrap().clone();
    assert!(
        ranges.iter().all(|&(from, _)| from > head),
        "after a restart with {kept} kept, payments were read again from {:?}",
        ranges.first()
    );
}
