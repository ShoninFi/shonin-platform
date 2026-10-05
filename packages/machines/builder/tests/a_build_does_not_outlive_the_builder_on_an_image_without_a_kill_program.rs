//! The builder's image carries no `kill` program (Debian ships one in
//! `procps`, which slim images leave out; a shell's `kill` is a builtin).
//! The build's process group is signalled from the build service's own
//! program, so with no `kill` on PATH nothing a build started outlives the
//! builder all the same.
//!
//! Sequence: the builder runs with a PATH holding only the stand-in `cargo`;
//! the author uploads and pays; the reviewer clears it; the stand-in `cargo`
//! starts a long-running process and waits; the builder is killed outright.
//! Within seconds, neither the stand-in `cargo` nor what it started is
//! running.

// Setting PATH for the builder this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::Address;

fn running(pid: &str) -> bool {
    std::process::Command::new("/bin/kill")
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
async fn a_build_does_not_outlive_the_builder_on_an_image_without_a_kill_program() {
    let bin = black_box::scratch().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let started_file = bin.join("started");
    let cargo = bin.join("cargo");
    std::fs::write(
        &cargo,
        format!(
            "#!/bin/sh\n/bin/sleep 120 &\necho $$ $! > {}\nwait\n",
            started_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Only the stand-in `cargo`: no `kill` program, as on the builder's image.
    // SAFETY: set once, before anything else in this test binary runs.
    unsafe { std::env::set_var("PATH", bin.display().to_string()) };
    black_box::CLEARS.store(true, Ordering::SeqCst);

    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0x79);
    let (status, body) = builder.upload(author, &black_box::project("no-kill")).await;
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
        let _ = std::process::Command::new("/bin/kill")
            .args(["-KILL", pid])
            .status();
    }
    assert!(
        left.is_empty(),
        "with no kill program on PATH, processes a build started outlived the builder: {left:?}"
    );
}
