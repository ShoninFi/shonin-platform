//! The build service runs on demand: started when there is work, it exits
//! once it has had nothing to do for a while, and the machine powers off.
//! Exiting with work in hand would leave a paid job waiting until something
//! else woke it, so "nothing to do" is narrow: no paid job waiting, being
//! reviewed or built, and no payment taken on a chain past those it has
//! read, however few blocks deep it is yet.
//!
//! Here the build service is told to exit after two idle seconds, on a chain
//! whose payments are read twelve blocks deep. It starts with a paid job
//! queued, which the reviewer refuses, and a payment the registry has taken
//! four blocks below the head, not yet deep enough to be read. It
//! must not exit while either is in hand; once the job has ended and the
//! payment has been read (admitting nothing), it exits, and cleanly.

mod black_box;

use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_build_service_exits_only_once_idle() {
    let author = Address::repeat_byte(0xa1);
    let queue = black_box::queue(vec![black_box::a_job(
        "8800",
        author,
        Some(1),
        JobState::Queued { position: 0 },
        time::Duration::ZERO,
    )]);
    let mut node = black_box::node(0x10, None);
    node.confirmations = Some(12);
    node.pay(0x02, 1, author, B256::repeat_byte(0x44));
    // A payment the registry has taken, not yet twelve blocks deep.
    node.pay(0x0c, 2, Address::repeat_byte(0xa2), B256::repeat_byte(0x45));
    let mut builder = black_box::spawn_with(node, &[queue], &["--idle-exit-seconds", "2"]).await;

    // The job ends, refused; the unread payment still keeps the build
    // service up, well past two idle seconds.
    let jobs = builder
        .wait_for(Duration::from_secs(30), |jobs| {
            jobs.iter().all(|job| job.state == JobState::Refused)
        })
        .await;
    assert!(
        jobs.iter().all(|job| job.state == JobState::Refused),
        "the job never ended:\n{}",
        builder.said.lock().unwrap()
    );
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        builder.child.try_wait().unwrap().is_none(),
        "the build service exited with a payment unread:\n{}",
        builder.said.lock().unwrap()
    );

    // The payment is deep enough; it is read, and admits nothing.
    builder
        .node
        .head
        .store(0x30, std::sync::atomic::Ordering::SeqCst);
    let started = Instant::now();
    let status = loop {
        if let Some(status) = builder.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the build service never exited once idle:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert!(
        status.success(),
        "the build service exited, but not cleanly ({status}):\n{}",
        builder.said.lock().unwrap()
    );
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "the build service exited before it had been idle for two seconds"
    );
}
