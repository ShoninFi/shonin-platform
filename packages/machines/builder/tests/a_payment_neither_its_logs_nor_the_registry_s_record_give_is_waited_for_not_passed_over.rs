//! A payment neither its block's logs nor the registry's record of it give
//! is waited for, never passed over: payments on that chain stop where they
//! are, the builder says so at warn, and it no longer counts that chain as
//! work in hand, so it can still go idle and exit rather than stay up for
//! a payment it can't read. The next start reads from where it stopped.
//!
//! Sequence: the chain stands at 0x40 and the build service exits after two
//! idle seconds. An author pays at 0x20 for an upload waiting since an hour
//! ago; no log of block 0x20 can be read, nor the registry's record of any
//! payment. The builder says payments there are not read, the upload still
//! waits, where payments were read to never passes 0x20, and the build
//! service exits, cleanly.

mod black_box;

use std::time::{Duration, Instant};

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_neither_its_logs_nor_the_registry_s_record_give_is_waited_for_not_passed_over() {
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
    node.record_unreadable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut builder = black_box::spawn_with(node, &[queue], &["--idle-exit-seconds", "2"]).await;

    let started = Instant::now();
    let status = loop {
        if let Some(status) = builder.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the build service stayed up for a payment it can't read:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let said = builder.said.lock().unwrap().clone();
    assert!(
        status.success(),
        "exited, but not cleanly ({status}):\n{said}"
    );
    assert!(
        said.contains("payments on this chain are not read, nor waited for"),
        "{said}"
    );
    assert!(
        builder
            .jobs()
            .iter()
            .all(|job| job.state == JobState::AwaitingPayment),
        "{:?}",
        builder.jobs()
    );
    let read: serde_json::Value = std::fs::read(builder.store().join("payments.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    assert!(
        read["base"].as_u64().unwrap_or(0) <= 0x20,
        "payments were read past the payment that could not be read: {read}"
    );
}
