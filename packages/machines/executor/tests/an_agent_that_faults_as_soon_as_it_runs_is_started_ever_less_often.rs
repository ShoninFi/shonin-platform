//! An agent that instantiates cleanly and traps the moment its `run` is
//! called is started again ever less often: 5 s, doubling to 5 min, as a
//! start that fails earlier is. Only an agent that has run for a good while
//! before it stops is started again after the first wait. And each start
//! uses the component as compiled before, compiling being the host's own
//! unbilled work.
//!
//! Sequence: an open vault, key blessed and approved, a tank well filled; its
//! component traps as soon as `run` is called. Over 45 s from its first
//! fault it is started at most four times more (t+5, t+15, t+35).

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
async fn an_agent_that_faults_as_soon_as_it_runs_is_started_ever_less_often() {
    let executor = black_box::start(TRAPS_IN_RUN, black_box::Script::Approved).await;

    // Wait for the first fault, then watch 45 s from there.
    let started = std::time::Instant::now();
    while executor.lines_with("the strategy stopped") == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the agent was never started:\n{}",
            executor.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(45)).await;
    let faults = executor.lines_with("the strategy stopped");
    let restarts = executor.lines_with("restarting the strategy");
    drop(executor);
    assert!(
        faults <= 5,
        "an agent that traps in `run` the moment it starts was started {faults} times in \
         45 s ({restarts} restarts): its restarts never back off past 5 s"
    );
}
