//! What a project is checked for before it is paid for (that it depends only
//! on crates inside it, the SDK and the platform's reviewed crates.io list,
//! and carries no Cargo config) is checked again, cheaply, before it is
//! compiled: the list may have changed since, and a queue may hold a job
//! admitted before a check was made. One that no longer passes fails as a
//! build, the compiler never run for it.
//!
//! Here `queue.json` holds a paid job whose lock file names a crates.io crate
//! the platform has not reviewed; the reviewer clears it.

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
async fn a_paid_project_is_checked_again_before_it_is_compiled() {
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

    let root = black_box::scratch().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"agent\"\n\n[dependencies]\nleft-pad = \"1\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("Cargo.lock"),
        black_box::lock(&["agent"], &[("left-pad", "1.0.0", &"ab".repeat(32))]),
    )
    .unwrap();
    std::fs::write(
        root.join("shonin.toml"),
        "[vault]\nchain = \"base\"\n[description]\ntitle = \"An agent\"\ntext = \"x\"\n\
         [drivers.local]\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), "// unreviewed\n").unwrap();
    let archive = shared::archive::pack(&root).unwrap();
    assert!(
        shared::dependencies::checked(
            &archive,
            "shonin.toml",
            &shared::dependencies::Allowlist::platform()
        )
        .is_err(),
        "the project would be refused before it is paid for"
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
            outcome: Failure::Build
        },
        "{}",
        builder.said.lock().unwrap()
    );
    assert!(
        !ran.exists(),
        "the compiler ran for a project depending on a crate the platform has not reviewed"
    );
}
