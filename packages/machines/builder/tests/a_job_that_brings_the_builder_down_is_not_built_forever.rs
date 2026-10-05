//! A job caught mid-build by a restart goes back in the queue, but
//! only so often: one whose build takes the builder down with it (its output
//! read whole into the builder's memory, a component Wasmtime takes forever
//! to load, or the OOM killer choosing the builder over the build) would
//! otherwise be built again after every restart, and take the builder down
//! again, so the one queue, every chain's, never got past it. Once a job has
//! been taken from the queue three times, each ended by a restart, it has
//! failed, and the queue moves on.
//!
//! Sequence: an author uploads a project and pays; the reviewer clears it.
//! Its build ends the builder's process (here a stand-in `cargo` kills its
//! parent, standing in for any of the above). The builder is restarted on the
//! same store, up to four times.

// Setting PATH for the builders this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::Address;

/// A build that takes the builder down with it.
const FAKE_CARGO: &str = "#!/bin/sh\nkill -9 $PPID\nsleep 5\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_that_brings_the_builder_down_is_not_built_forever() {
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

    let author = Address::repeat_byte(0x77);
    let mut builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let (status, body) = builder.upload(author, &black_box::project("poison")).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    builder
        .node
        .pay(0x11, 5, author, accepted.upload.parse().unwrap());
    builder.node.head.store(0x12, Ordering::SeqCst);

    let mut deaths = 0;
    for round in 0..5 {
        // Wait for this builder to die, or to end the job.
        let started = Instant::now();
        let died = loop {
            if builder.child.try_wait().unwrap().is_some() {
                break true;
            }
            if builder
                .jobs()
                .iter()
                .any(|job| job.id == accepted.job_id && job.state.is_finished())
            {
                break false;
            }
            assert!(
                started.elapsed() < Duration::from_secs(40),
                "round {round}: neither died nor ended the job\n{}",
                builder.said.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        if !died {
            break;
        }
        deaths += 1;
        // Restart on the same store: its queue, with the job's archive put
        // back inline (a form the builder still reads), and its payments.
        let mut queue: serde_json::Value = builder.queue_file();
        let archive = std::fs::read(
            builder
                .store()
                .join("archives")
                .join(format!("{}.tar", accepted.job_id)),
        )
        .unwrap();
        for entry in queue["jobs"].as_array_mut().unwrap() {
            if entry["job"]["id"] == accepted.job_id.as_str() {
                entry["archive"] = serde_json::json!(shared::base64::encode(&archive));
            }
        }
        let payments = std::fs::read_to_string(builder.store().join("payments.json")).unwrap();
        let node = black_box::node(0x12, None);
        node.pay(0x11, 5, author, accepted.upload.parse().unwrap());
        builder = black_box::spawn(
            node,
            &[
                ("queue.json", queue.to_string()),
                ("payments.json", payments),
            ],
        )
        .await;
    }
    assert!(
        deaths < 5,
        "the job took the builder down {deaths} times running, and is built again after every \
         restart; nothing behind it is ever built"
    );
}
