//! Cargo reads more from the directories above a build than a `.cargo`: it
//! walks every ancestor of the project for a `Cargo.toml` holding
//! `[workspace]` and, finding one, makes the project a member of that
//! workspace, taking its `[patch]`, its `[profile]` and its lock file. It
//! also reads a `.cargo` in every ancestor up to the root.
//!
//! Each job is built in a directory made fresh for it, inside the build
//! service's own jobs directory; but a build runs as the build service does,
//! so an earlier build's script can leave either file above a later one.
//! No job is to blame for that, so no job fails for it: while either lies
//! above the jobs directory, the build service takes no job from the queue,
//! its health route answers 503 naming the file, and it says so at error.
//! Every job keeps its place and its payment, and is built once the file is
//! gone.
//!
//! Sequence (first test): an earlier job's `build.rs` writes
//! `<jobs>/Cargo.toml` reading `[workspace]` (simulated here by writing it
//! directly); author B uploads an ordinary project, pays, and the reviewer
//! would clear it. Expected: the job waits in the queue, the compiler not
//! run, until the file is removed; then it is built.
//!
//! Sequence (second test): a `.cargo` lies in the directory holding the jobs
//! directory, which real cargo reads for every build. Expected: the same.

// Setting PATH for the builder this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::time::Duration;

use alloy::primitives::Address;
use shared::Failure;
use shared::model::registry::JobState;

/// The stand-in `cargo`, which notes the directory it was run in, once per
/// test binary.
fn stand_in() -> &'static PathBuf {
    static RAN: OnceLock<PathBuf> = OnceLock::new();
    RAN.get_or_init(|| {
        let bin = black_box::scratch().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let ran = bin.join("ran");
        let cargo = bin.join("cargo");
        std::fs::write(
            &cargo,
            format!("#!/bin/sh\necho \"$PWD\" >> {}\nexit 1\n", ran.display()),
        )
        .unwrap();
        std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        // SAFETY: set once, before any builder in this test binary starts.
        unsafe { std::env::set_var("PATH", path) };
        black_box::CLEARS.store(true, Ordering::SeqCst);
        ran
    })
}

/// Whether the stand-in compiler has run for a build in `builder`'s jobs
/// directory.
fn compiler_ran(builder: &black_box::Builder) -> bool {
    let jobs = builder.dir.join("jobs");
    let real = jobs.canonicalize().unwrap_or(jobs.clone());
    std::fs::read_to_string(stand_in())
        .unwrap_or_default()
        .lines()
        .any(|dir| {
            let dir = Path::new(dir);
            dir.starts_with(&real) || dir.starts_with(&jobs)
        })
}

/// Upload and pay for an ordinary project after `leave` has put something
/// above its build (given the directory holding the jobs directory, and
/// saying what it left), and check the queue is held while it is there and
/// let go once it is removed.
async fn held_while_left(author: Address, leave: impl FnOnce(&Path) -> PathBuf) {
    stand_in();
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let left = leave(&builder.dir);

    let (status, body) = builder
        .upload(author, &black_box::project(&format!("{author:?}")))
        .await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    builder
        .node
        .pay(0x11, 5, author, accepted.upload.parse().unwrap());
    builder.node.head.store(0x12, Ordering::SeqCst);

    // Held: the job is paid for and waits, the compiler never runs, and the
    // health route and the log name what holds it.
    let paid = builder
        .wait_for(Duration::from_secs(30), |jobs| {
            jobs.iter().any(|job| {
                job.id == accepted.job_id && matches!(job.state, JobState::Queued { .. })
            })
        })
        .await;
    assert!(
        paid.iter()
            .any(|job| job.id == accepted.job_id && matches!(job.state, JobState::Queued { .. })),
        "the job was never queued: {paid:?}"
    );
    tokio::time::sleep(Duration::from_secs(5)).await;
    let job = builder.job(&accepted.job_id).await.unwrap();
    assert!(
        matches!(job.state, JobState::Queued { .. }),
        "a job was taken from the queue while {} lay above the jobs directory: {:?}\n{}",
        left.display(),
        job.state,
        builder.said.lock().unwrap()
    );
    assert!(
        !compiler_ran(&builder),
        "the compiler ran with {} above the build",
        left.display()
    );
    let health = reqwest::get(format!("{}/health", builder.url))
        .await
        .unwrap();
    assert_eq!(health.status(), 503);
    let said = health.text().await.unwrap();
    assert!(said.contains(&left.display().to_string()), "{said}");
    assert!(
        builder
            .said
            .lock()
            .unwrap()
            .contains(&left.display().to_string()),
        "the log does not name {}",
        left.display()
    );

    // Let go once it is removed: the job is built, and (the stand-in
    // compiler failing) fails as a build, nothing else.
    let _ = std::fs::remove_dir_all(&left);
    let _ = std::fs::remove_file(&left);
    let jobs = builder
        .wait_for(Duration::from_secs(60), |jobs| {
            jobs.iter()
                .any(|job| job.id == accepted.job_id && job.state.is_finished())
        })
        .await;
    let ended = jobs
        .iter()
        .find(|job| job.id == accepted.job_id)
        .expect("the upload is a job");
    assert_eq!(
        ended.state,
        JobState::Failed {
            outcome: Failure::Build
        },
        "the job was not built once {} was removed:\n{}",
        left.display(),
        builder.said.lock().unwrap()
    );
    assert!(compiler_ran(&builder), "the compiler never ran");
    let health = reqwest::get(format!("{}/health", builder.url))
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workspace_root_left_above_a_build_holds_the_queue() {
    held_while_left(Address::repeat_byte(0x61), |dir| {
        // What an earlier job's build script can write: every later
        // project with a `[package]` at its root becomes a member of this
        // workspace, or fails as not one.
        let left = dir.join("jobs/Cargo.toml");
        std::fs::write(
            &left,
            "[workspace]\nmembers = [\"*/source\"]\n\n[patch.crates-io]\n\
             shonin-strategy-sdk = { path = \"/somewhere/else\" }\n",
        )
        .unwrap();
        left
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cargo_config_above_the_jobs_directory_holds_the_queue() {
    held_while_left(Address::repeat_byte(0x62), |dir| {
        let left = dir.join(".cargo");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::write(
            left.join("config.toml"),
            "[build]\nrustc-wrapper = \"/somewhere/else\"\n",
        )
        .unwrap();
        left
    })
    .await;
}
