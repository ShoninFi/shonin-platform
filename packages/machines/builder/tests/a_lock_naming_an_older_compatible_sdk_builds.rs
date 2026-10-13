//! A lock made before the build service took a newer SDK names an earlier
//! version of it, which the build service's copy is compatible with: the
//! agent is built against that copy, as Cargo would build it had the author
//! updated the SDK, and the lock's entry is made the path package at the
//! carried version.
//!
//! Sequence: author A uploads an agent whose lock takes the SDK from
//! crates.io at an earlier version compatible with the carried one, and
//! pays; the reviewer clears it. Expected: the upload is taken, the stand-in
//! `cargo` is run on a lock naming the SDK as the path package at the carried
//! version, and the job is ready.

// Setting PATH for the builder this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::sync::atomic::Ordering;

use alloy::primitives::Address;
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lock_naming_an_older_compatible_sdk_builds() {
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
    let parts: Vec<&str> = carried.split('.').collect();
    let earlier = format!("{}.{}.0", parts[0], parts[1]);
    assert_ne!(
        earlier, carried,
        "the carried SDK has no earlier version compatible with it to lock"
    );
    let archive = black_box::agent_on_the_sdk(
        &earlier,
        &black_box::lock_with_sdk(&black_box::sdk_from_crates_io(&earlier, &"6".repeat(64))),
    );
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let (job, said) = builder.publish(Address::repeat_byte(0x79), &archive).await;
    assert_eq!(
        job.state,
        JobState::Ready,
        "an agent locking an earlier compatible SDK from crates.io did not build:\n{said}"
    );
    assert_eq!(
        std::fs::read_to_string(&kept).unwrap(),
        black_box::lock_with_sdk(&black_box::sdk_as_carried(carried)),
        "cargo was not run on the lock with the SDK as the build service's own copy"
    );
}
