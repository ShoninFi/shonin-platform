//! A paid project is checked again before it is compiled. One refused only
//! for how it names the SDK passed that check when it was paid for, so what
//! changed is the SDK the build service carries, not the project: the job
//! fails on the build service's side, never charged to the author as a
//! failed build, and the compiler never runs for it.
//!
//! Here `queue.json` holds a paid job whose lock names the SDK from crates.io
//! at an earlier version than the one the build service now carries, as a
//! build service carrying that version took it; the reviewer clears it.

// Setting PATH for the builder this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;
use std::time::Duration;

use alloy::primitives::Address;
use shared::Failure;
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paid_project_refused_only_for_the_sdk_the_build_service_now_carries_fails_on_its_side() {
    let bin = black_box::scratch().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ran = bin.join("ran");
    let cargo = bin.join("cargo");
    std::fs::write(
        &cargo,
        format!("#!/bin/sh\necho ran >> {}\nexit 1\n", ran.display()),
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // SAFETY: set once, before the builder this test starts.
    unsafe { std::env::set_var("PATH", path) };
    black_box::CLEARS.store(true, Ordering::SeqCst);

    let carried = strategy_sdk::VERSION;
    let parts: Vec<&str> = carried.split('.').collect();
    let earlier = format!("{}.{}.0", parts[0], parts[1]);
    assert_ne!(earlier, carried);
    let archive = black_box::agent_on_the_sdk(
        &earlier,
        &black_box::lock_with_sdk(&black_box::sdk_from_crates_io(&earlier, &"5".repeat(64))),
    );
    let sdk_refused = shared::dependencies::sdk_refused(
        &archive,
        &shared::dependencies::Allowlist::platform()
            .carrying(
                carried,
                ["ciborium", "fastnum", "futures", "serde", "wit-bindgen"],
            )
            .unwrap(),
    );
    assert!(
        sdk_refused.is_some(),
        "the project names the SDK the build service carries"
    );

    let mut job = black_box::a_job(
        "8800",
        Address::repeat_byte(0xa7),
        Some(1),
        JobState::Queued { position: 0 },
        time::Duration::ZERO,
    );
    job["job"]["source_hash"] = shared::manifest::hash_bytes(&archive).into();
    job["archive"] = shared::base64::encode(&archive).into();
    let builder =
        black_box::start(black_box::node(0x10, None), &[black_box::queue(vec![job])]).await;
    let jobs = builder
        .wait_for(Duration::from_secs(30), |jobs| {
            jobs.iter().all(|job| job.state.is_finished())
        })
        .await;
    assert_eq!(builder.reviewed.lock().unwrap().as_slice(), [1]);
    assert_eq!(
        jobs[0].state,
        JobState::Failed {
            outcome: Failure::Service
        },
        "{}",
        builder.said.lock().unwrap()
    );
    assert!(
        !ran.exists(),
        "the compiler ran for a project naming an SDK the build service does not carry"
    );
}
