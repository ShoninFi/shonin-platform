//! An agent that faults as soon as it runs is started ever less often (5 s
//! doubling to 5 min, reset only after a minute of clean running), even when
//! a read fails between one fault and the next start and ends the vault's
//! serve. The count of failed starts is the vault's, not the serve's: a serve
//! that ends for any other reason neither resets it nor starts the agent
//! early.
//!
//! Sequence: an open vault, key blessed and approved, tank full; the agent
//! traps the moment `run` is called. The node fails `state()` for one second
//! after each `haltedAt()` (the last read before an agent starts), so each
//! fault is followed by a failed read and the serve ends. Over the minute
//! from its first fault the agent is started at most four times more (t+5,
//! t+15, t+35), not every few seconds.

mod black_box;

use std::time::Duration;

/// A component that instantiates cleanly and traps as soon as `run` is
/// called.
const TRAPS_IN_RUN: &str = r#"(component
    (core module $m
        (memory (export "memory") 1)
        (func (export "run") (result i32) unreachable))
    (core instance $i (instantiate $m))
    (func $run async (result (result (error string)))
        (canon lift (core func $i "run") (memory (core memory $i "memory"))))
    (export "run" (func $run)))"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_that_fails_between_faults_does_not_undo_the_restart_backoff() {
    let executor = black_box::start_with(
        TRAPS_IN_RUN,
        black_box::Script::Approved,
        Some(Duration::from_secs(1)),
    )
    .await;

    let started = std::time::Instant::now();
    while executor.lines_with("the strategy stopped") == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the agent was never started:\n{}",
            executor.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(60)).await;
    let faults = executor.lines_with("the strategy stopped");
    let serve_errors = executor.lines_with("stopped serving this vault");
    let hiccups = *executor.node.hiccups.lock().unwrap();
    drop(executor);
    // Started at 0, then 5, 15 and 35 s on if the doubling held: at most five
    // faults in the minute, counting the first.
    assert!(
        faults <= 5,
        "an agent that traps the moment it runs was started {faults} times in 60 s: each of \
         {serve_errors} serves that ended on a failed read ({hiccups} reads failed) started \
         its backoff again from 5 s"
    );
}
