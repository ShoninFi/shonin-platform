//! An author depends on the SDK from crates.io as on any crate, and commits
//! the lock Cargo writes for it: no patch pointing at a copy of the
//! platform. The lock only names the SDK; the build service decides what the
//! SDK is, so it compiles the agent against its own copy, the lock's entry
//! for the SDK made the path package that copy is, whatever checksum the
//! lock held.
//!
//! Sequence: author A uploads an agent whose lock takes the SDK from
//! crates.io at the version the build service carries, and pays; the
//! reviewer clears it. Expected: the upload is taken, the stand-in `cargo` is
//! run on a lock naming the SDK as the path package at the carried version,
//! and the job is ready.

// Setting PATH for the builder this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::sync::atomic::Ordering;

use alloy::primitives::Address;
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lock_taking_the_sdk_from_crates_io_builds() {
    let bin = black_box::scratch().join("bin");
    let kept = bin.join("Cargo.lock");
    black_box::cargo_keeping_the_lock(&bin, &kept);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // SAFETY: set once, before anything else in this test binary runs.
    unsafe { std::env::set_var("PATH", path) };
    black_box::CLEARS.store(true, Ordering::SeqCst);

    let carried = strategy_sdk::VERSION;
    let archive = black_box::agent_on_the_sdk(
        carried,
        &black_box::lock_with_sdk(&black_box::sdk_from_crates_io(carried, &"5".repeat(64))),
    );
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let (job, said) = builder.publish(Address::repeat_byte(0x78), &archive).await;
    assert_eq!(
        job.state,
        JobState::Ready,
        "an agent locking the SDK from crates.io did not build:\n{said}"
    );

    let built_from = std::fs::read_to_string(&kept).unwrap();
    assert_eq!(
        built_from,
        black_box::lock_with_sdk(&black_box::sdk_as_carried(carried)),
        "cargo was not run on the lock with the SDK as the build service's own copy"
    );
}
