//! A payment behind a brief outage is admitted once the node reads again,
//! in the same run. A read that fails for the node's own reasons (a rate
//! limit, an endpoint behind, the past state it no longer keeps, an outage)
//! is a turn that failed, tried again soon and then less often: it never
//! stops payments on that chain for the run, and while it is tried again
//! soon, a payment that may wait behind it keeps the build service up.
//!
//! Sequence: the chain stands at 0x40 and the build service exits after five
//! idle seconds. An author pays at 0x20 for an upload waiting since an hour
//! ago; no log of block 0x20 can be read, nor the registry's record of any
//! payment. Once the builder says it could not read payments, the node
//! recovers at once: the job is admitted before the build service exits.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_behind_a_brief_outage_is_admitted_once_the_node_reads_again() {
    let author = Address::repeat_byte(0x77);
    let reference = B256::repeat_byte(0x44);
    let archive = black_box::project("paid");
    let queue = black_box::queue(vec![black_box::waiting(
        "8800",
        author,
        reference,
        &archive,
        time::Duration::hours(1),
    )]);
    let node = black_box::node(0x40, None);
    node.pay(0x20, 1, author, reference);
    node.unreadable.lock().unwrap().push(0x20);
    node.record_unreadable.store(true, Ordering::SeqCst);
    let mut builder = black_box::spawn_with(node, &[queue], &["--idle-exit-seconds", "5"]).await;

    let started = Instant::now();
    while !builder
        .said
        .lock()
        .unwrap()
        .contains("could not read payments")
    {
        assert!(
            builder.child.try_wait().unwrap().is_none(),
            "the build service exited before it read payments:\n{}",
            builder.said.lock().unwrap()
        );
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the read never failed:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The outage ends at once.
    builder.node.unreadable.lock().unwrap().clear();
    builder
        .node
        .record_unreadable
        .store(false, Ordering::SeqCst);

    let recovered = Instant::now();
    loop {
        let admitted = builder
            .jobs()
            .iter()
            .any(|job| job.id == "8800" && job.state != JobState::AwaitingPayment);
        if admitted {
            break;
        }
        assert!(
            builder.child.try_wait().unwrap().is_none(),
            "the node read again, yet the build service exited with the paid job unadmitted: \
             {:?}\n{}",
            builder.jobs(),
            builder.said.lock().unwrap()
        );
        assert!(
            recovered.elapsed() < Duration::from_secs(60),
            "the paid job was never admitted: {:?}\n{}",
            builder.jobs(),
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
