//! A job whose registration the registry already holds reads ready with the
//! bundle the registry holds, not the one the builder has just built, and
//! only if the store holds that bundle's files; otherwise it has failed. The
//! two bundles differ whenever a restart caught the job while its first
//! registration was still in flight: the job is built again (a build script
//! may stamp a time), the second registration is refused as registered
//! already, and a job reading ready with the second bundle would name one
//! the chain never bound and no vault will ever run.
//!
//! Sequence: two builders each hold job 8800 (strategy 5) as building, its
//! registration sent before they stopped. On restart the registry does not
//! hold strategy 5 yet, so each queues the job again; the first registration
//! then lands (bundle B1). The job is reviewed and built again, producing
//! B2, and its registration is refused. Where the store holds B1's files (as
//! the first run wrote them before it registered), the job is ready with B1;
//! where it does not, the job has failed.

// Setting PATH for the builders this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;
use std::time::Duration;

use alloy::primitives::{Address, B256};
use shared::Failure;
use shared::model::registry::JobState;

/// A stand-in for `cargo build` whose output differs every run, as a build
/// that stamps the time does.
const FAKE_CARGO: &str = r#"#!/bin/sh
mkdir -p "$CARGO_TARGET_DIR/wasm32-wasip2/release"
# An empty component, then a custom section ("x") holding ten digits of
# this run's own (its process id).
{ printf '\000asm\015\000\001\000'; printf '\000\014\001x'; printf '%010d' $$; } \
  > "$CARGO_TARGET_DIR/wasm32-wasip2/release/agent.wasm"
"#;

/// Start a builder holding job 8800 as building, with `files` in its store,
/// and have the registration sent before the restart land once it is up.
async fn caught_mid_registration(files: Vec<(&'static str, String)>) -> black_box::Builder {
    let author = Address::repeat_byte(0x77);
    let archive = black_box::project("registered once already");
    let mut entry = black_box::waiting(
        "8800",
        author,
        B256::repeat_byte(0x44),
        &archive,
        time::Duration::minutes(5),
    );
    entry["job"]["strategy_id"] = serde_json::json!(5);
    entry["job"]["state"] = serde_json::to_value(JobState::Building).unwrap();
    let mut files = files;
    files.push(black_box::queue(vec![entry]));

    // The registry doesn't hold strategy 5 when the builder starts, and the
    // registration sent before the restart lands just after.
    let node = black_box::node(0x10, None);
    node.landing.lock().unwrap().push(5);
    black_box::start(node, &files).await
}

async fn ended(builder: &black_box::Builder) -> (JobState, Option<String>) {
    let jobs = builder
        .wait_for(Duration::from_secs(20), |jobs| {
            jobs.iter()
                .any(|job| job.id == "8800" && job.state.is_finished())
        })
        .await;
    let job = jobs.iter().find(|job| job.id == "8800").unwrap();
    (job.state.clone(), job.bundle_hash.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_reads_ready_only_with_the_bundle_the_registry_holds() {
    let bin = black_box::scratch().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let cargo = bin.join("cargo");
    std::fs::write(&cargo, FAKE_CARGO).unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // SAFETY: set once, before anything else in this test binary runs.
    unsafe { std::env::set_var("PATH", path) };
    black_box::CLEARS.store(true, Ordering::SeqCst);

    let stored = caught_mid_registration(black_box::stored(black_box::REGISTERED_BUNDLE)).await;
    let unstored = caught_mid_registration(Vec::new()).await;
    let (stored_ended, unstored_ended) = tokio::join!(ended(&stored), ended(&unstored));

    let registered = format!(
        "sha256:{}",
        alloy::hex::encode(black_box::REGISTERED_BUNDLE)
    );
    assert_eq!(
        stored_ended,
        (JobState::Ready, Some(registered)),
        "the job does not read ready with the bundle the registry holds\n{}",
        stored.said.lock().unwrap()
    );
    assert_eq!(
        unstored_ended,
        (
            JobState::Failed {
                outcome: Failure::Service
            },
            None
        ),
        "the job reads as if it were ready with a bundle the store does not hold\n{}",
        unstored.said.lock().unwrap()
    );
}
