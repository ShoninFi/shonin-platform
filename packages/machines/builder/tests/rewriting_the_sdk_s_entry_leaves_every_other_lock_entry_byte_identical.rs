//! Before it builds, the build service makes a lock's entry for the SDK from
//! crates.io, at the carried version or an earlier one, the path package it
//! patches in. That entry is the only thing it touches: every other entry,
//! and every other byte of the lock (its line endings, comments and spacing
//! included), is what the author committed and the reviewer was told of.
//!
//! Sequence: author A uploads an agent whose lock takes the SDK from
//! crates.io at the carried version, then one at an earlier compatible
//! version, each lock written with CRLF line endings, comments,
//! and other entries spaced as Cargo would not write them; pays for each;
//! the reviewer clears them. Expected: for each, the stand-in `cargo` is run
//! on a lock that differs from the upload's only in the SDK's entry, made the
//! path package at the carried version, and the job is ready.

// Setting PATH for the builder this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::sync::atomic::Ordering;

use alloy::primitives::Address;
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rewriting_the_sdk_s_entry_leaves_every_other_lock_entry_byte_identical() {
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
    let crlf = |text: &str| text.replace('\n', "\r\n");
    let by_path = crlf(&black_box::sdk_as_carried(carried));
    let entries = [
        (
            "at the carried version",
            crlf(&black_box::sdk_from_crates_io(carried, &"9".repeat(64))),
        ),
        (
            "at an earlier version",
            crlf(&black_box::sdk_from_crates_io(&earlier, &"9".repeat(64))),
        ),
    ];

    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    for (author, (what, entry)) in entries.iter().enumerate() {
        let uploaded = crlf(
            &black_box::lock_with_sdk("@SDK@")
                .replace("name = \"futures\"", "name   =   \"futures\"  # spaced\t")
                .replace(
                    "[[package]]\nname = \"serde\"",
                    "# a comment\n\n[[package]]\nname = 'serde'",
                )
                .replace(
                    "dependencies = [\n \"futures\",",
                    "dependencies = [  \"futures\",",
                ),
        )
        .replace("@SDK@", entry);
        let expected = uploaded.replace(entry, &by_path);
        assert_ne!(uploaded, expected);

        let archive = black_box::agent_on_the_sdk(carried, &uploaded);
        let (job, said) = builder
            .publish(Address::repeat_byte(0x7d + author as u8), &archive)
            .await;
        assert_eq!(
            job.state,
            JobState::Ready,
            "an agent locking the SDK {what} did not build:\n{said}"
        );
        assert_eq!(
            std::fs::read_to_string(&kept).unwrap(),
            expected,
            "with the SDK locked {what}, the lock built from differs from the one uploaded \
             beyond the SDK's entry"
        );
    }
}
