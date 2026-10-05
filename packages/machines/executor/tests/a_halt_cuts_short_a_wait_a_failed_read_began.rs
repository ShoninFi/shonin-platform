//! A faulting agent's held-off restart is cut short by its vault's halt
//! however the wait began: by the serve itself, or by the sweep, after a read
//! that failed between a fault and the next start ended the serve: the wait
//! ends early when its vault halts.
//!
//! Sequence: an open vault, key blessed and approved, tank full; the agent
//! traps as soon as `run` is called. The node fails `state()` for one second
//! after each `haltedAt()` (as `a_read_that_fails_between_faults...` does), so
//! each fault is followed by a failed read in `after_the_agent`, the serve
//! ends with an error, and the sweep holds the vault off 5 s, 10 s, 20 s. Two
//! seconds into the 20 s wait the vault halts, and the agent is started again
//! ten seconds into the wait, not once the 20 s are up.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const TRAPS_IN_RUN: &str = r#"(component
    (core module $m
        (memory (export "memory") 1)
        (func (export "run") (result i32) unreachable))
    (core instance $i (instantiate $m))
    (func $run async (result (result (error string)))
        (canon lift (core func $i "run") (memory (core memory $i "memory"))))
    (export "run" (func $run)))"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_halt_cuts_short_a_wait_a_failed_read_began() {
    let executor = black_box::start_with(
        TRAPS_IN_RUN,
        black_box::Script::Approved,
        Some(Duration::from_secs(1)),
    )
    .await;

    let started = Instant::now();
    while executor.lines_with("the strategy stopped") < 3 {
        assert!(
            started.elapsed() < Duration::from_secs(120),
            "the agent did not fault three times:\n{}",
            executor.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        executor.lines_with("stopped serving this vault") >= 2,
        "the serves did not end on the failed read, so this is not the case under test:\n{}",
        executor.said.lock().unwrap()
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(executor.lines_with("the strategy stopped"), 3);

    executor
        .node
        .halted_at
        .store(black_box::NOW, Ordering::SeqCst);
    let halted = Instant::now();
    while executor.lines_with("the strategy stopped") < 4 {
        assert!(
            halted.elapsed() < Duration::from_secs(15),
            "the vault halted during a wait begun by a failed read, and the agent was not \
             started again for {:?} of its grace:\n{}",
            halted.elapsed(),
            executor.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
