//! A build's process group is killed when the build ends, however it ends,
//! not only when it is given up on: anything a build script started in that
//! group (no `setsid` needed) ends with the build, rather than running on
//! past the job and past the build service.
//!
//! Sequence: the author uploads and pays; the reviewer clears it; the
//! stand-in `cargo` starts a process in the background (as a `build.rs` can)
//! and exits, failing the build; the job ends failed, and the background
//! process with it, while the builder runs on.

// Setting PATH for the builder this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use shared::model::registry::JobState;

fn running(pid: &str) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_a_finished_build_left_running_does_not_outlive_the_builder() {
    let bin = black_box::scratch().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let started_file = bin.join("started");
    let cargo = bin.join("cargo");
    std::fs::write(
        &cargo,
        format!(
            "#!/bin/sh\n/bin/sleep 120 &\necho $! > {}\nexit 1\n",
            started_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // SAFETY: set once, before anything else in this test binary runs.
    unsafe { std::env::set_var("PATH", path) };
    black_box::CLEARS.store(true, Ordering::SeqCst);

    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0x78);
    let (status, body) = builder.upload(author, &black_box::project("left")).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    builder
        .node
        .pay(0x11, 5, author, accepted.upload.parse().unwrap());
    builder.node.head.store(0x12, Ordering::SeqCst);

    let jobs = builder
        .wait_for(Duration::from_secs(30), |jobs| {
            jobs.iter()
                .any(|job| matches!(job.state, JobState::Failed { .. }))
        })
        .await;
    assert!(
        jobs.iter()
            .any(|job| matches!(job.state, JobState::Failed { .. })),
        "the build never ended: {jobs:?}\n{}",
        builder.said.lock().unwrap()
    );
    let pid = std::fs::read_to_string(&started_file)
        .unwrap()
        .trim()
        .to_string();

    // Ended with the build, while the builder runs on.
    let ended = Instant::now();
    while running(&pid) && ended.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let outlived_the_build = running(&pid);

    drop(builder);
    let killed = Instant::now();
    while running(&pid) && killed.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let outlived_the_builder = running(&pid);
    let _ = std::process::Command::new("/bin/kill")
        .args(["-KILL", &pid])
        .status();
    assert!(
        !outlived_the_build,
        "a process the build started outlived the build"
    );
    assert!(
        !outlived_the_builder,
        "a process the build started outlived the builder that started it"
    );
}
