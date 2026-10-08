//! Nothing a build started outlives the build, in its process group or
//! not. A build step's process group is killed when the step ends; a build
//! script can start a process in a session of its own (`setsid`, or
//! `process_group(0)` from Rust's standard library), outside that group.
//! Where builds run as a user of their own (the build service run as root,
//! as in the attested image), every process of that user is killed once a
//! step is done, so such a process ends with the build too.
//!
//! Sequence: the author uploads and pays; the reviewer clears it; the
//! stand-in `cargo` starts `sleep` in a new session (through perl's
//! `POSIX::setsid`, as a build script would through `setsid(2)`), writes
//! its pid, and exits failing the build. The job ends failed, and the
//! sleeper must have ended with it, while the builder runs on.
//!
//! Run as anyone but root, builds run as the build service's own user, which
//! the builder can't kill everything of: the test then has nothing to show,
//! and says so.

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
async fn a_build_s_process_in_a_session_of_its_own_does_not_outlive_the_build() {
    if !rustix::process::geteuid().is_root() {
        eprintln!("not root: builds run as this user, and are not swept; nothing to show");
        return;
    }
    let perl = ["/usr/bin/perl", "/bin/perl", "/usr/local/bin/perl"]
        .into_iter()
        .find(|perl| std::path::Path::new(perl).is_file())
        .expect("perl, to start a process in a session of its own as setsid(2) would");
    let bin = black_box::scratch().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // The build user runs the stand-in and writes its pid beside it.
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o777)).unwrap();
    let started_file = bin.join("started");
    let cargo = bin.join("cargo");
    // The sleeper leaves the build's process group for a session of its own
    // (then execs, so its pid is the one written), as a build script can.
    std::fs::write(
        &cargo,
        format!(
            "#!/bin/sh\n{perl} -MPOSIX -e 'POSIX::setsid(); exec \"/bin/sleep\", \"120\"' &\n\
             echo $! > {}\n/bin/sleep 1\nexit 1\n",
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
    let author = Address::repeat_byte(0x79);
    let (status, body) = builder.upload(author, &black_box::project("session")).await;
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
    assert!(!pid.is_empty(), "the stand-in cargo wrote no pid");

    // Ended with the build, while the builder runs on.
    let ended = Instant::now();
    while running(&pid) && ended.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let outlived_the_build = running(&pid);
    drop(builder);
    let _ = std::process::Command::new("/bin/kill")
        .args(["-KILL", &pid])
        .status();
    assert!(
        !outlived_the_build,
        "a process the build started in a session of its own outlived the build"
    );
}
