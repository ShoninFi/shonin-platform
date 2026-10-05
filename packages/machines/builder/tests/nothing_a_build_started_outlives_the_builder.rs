//! A build runs the compiler, and the compiler runs build scripts and
//! compilers of its own. Should the build service die mid-build, none of
//! them is left running: the compiler is run in a process group of its own
//! under a watcher that kills that whole group once the build service is
//! gone.
//!
//! Sequence: the author uploads a project and pays; the reviewer clears it;
//! the stand-in `cargo` starts a long-running process of its own and waits.
//! The builder is killed outright. Within seconds, neither the stand-in
//! `cargo` nor what it started is running.

// Setting PATH for the builder this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::Address;

fn running(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn pids(file: &Path) -> Vec<String> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_a_build_started_outlives_the_builder() {
    let bin = black_box::scratch().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let started_file = bin.join("started");
    let cargo = bin.join("cargo");
    std::fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nsleep 120 &\necho $$ $! > {}\nwait\n",
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
    let author = Address::repeat_byte(0x77);
    let (status, body) = builder
        .upload(author, &black_box::project("outlived"))
        .await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    builder
        .node
        .pay(0x11, 5, author, accepted.upload.parse().unwrap());
    builder.node.head.store(0x12, Ordering::SeqCst);

    let started = Instant::now();
    while pids(&started_file).len() < 2 {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the build never started:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let pids = pids(&started_file);
    assert!(pids.iter().all(|pid| running(pid)));

    drop(builder);
    let killed = Instant::now();
    while pids.iter().any(|pid| running(pid)) && killed.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let left: Vec<&String> = pids.iter().filter(|pid| running(pid)).collect();
    for pid in &left {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", pid])
            .status();
    }
    assert!(
        left.is_empty(),
        "processes a build started outlived the builder that started them: {left:?}"
    );
}
