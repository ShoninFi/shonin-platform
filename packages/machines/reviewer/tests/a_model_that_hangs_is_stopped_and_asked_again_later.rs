//! The build service waits for the reviewer for a while, then asks again. A
//! model that never answers must not outlast that wait: each ask would start
//! another model process beside the ones still hanging. So the model is given
//! a time to answer in, shorter than the build service's wait; past it, the
//! model's program and everything it started are stopped, and the review is
//! answered 503 (ask again later), the reviewer's own trouble.
//!
//! Here the model's program starts a process of its own and then never
//! answers, and the reviewer gives its model two seconds.

mod black_box;

use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_that_hangs_is_stopped_and_asked_again_later() {
    let bin = black_box::a_claude("");
    let started_one = bin.join("started");
    std::fs::write(
        bin.join("claude"),
        format!(
            "#!/bin/sh\ncat > /dev/null\nsleep 600 &\necho $! > {0}.part\nmv {0}.part {0}\n\
             wait\n",
            started_one.display()
        ),
    )
    .unwrap();
    let reviewer = black_box::start(black_box::Options {
        node: true,
        path_first: Some(&bin),
        model: Some("stand-in-model"),
        args: vec!["--model-seconds", "2"],
        ..Default::default()
    })
    .await;

    let asked = Instant::now();
    let (status, body) = reviewer.review(7, &black_box::project("hangs")).await;
    assert_eq!(
        status,
        503,
        "a review the model never answered was not answered ask-again-later: {body}\n{}",
        reviewer.said.lock().unwrap()
    );
    assert!(
        asked.elapsed() < Duration::from_secs(60),
        "the reviewer waited past its model's time: {:?}",
        asked.elapsed()
    );

    let pid = std::fs::read_to_string(&started_one).unwrap();
    let pid = pid.trim();
    let gone = Instant::now();
    loop {
        let alive = std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        if !alive {
            break;
        }
        if gone.elapsed() > Duration::from_secs(10) {
            // Not left for the next test run either.
            let _ = std::process::Command::new("kill")
                .args(["-9", pid])
                .status();
            panic!("a process the hanging model started outlived its review");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
