//! An agent held off after faulting is started again as soon as its vault
//! halts (though never sooner than ten seconds after it was last started),
//! rather than when its wait (up to five minutes) is over: the halt's
//! grace is the agent's time to wind down in, and it is only so long. The
//! same wait is cut short by anything else the serve acts on: a change to
//! the allowances, the vault closing or its grace ending, or another
//! executor taking it over.
//!
//! Sequence: an open vault, key blessed and approved, a tank well filled; its
//! component traps as soon as `run` is called, so it is started again after
//! 5 s, then 10 s, then 20 s. Two seconds into the 20 s wait the vault halts,
//! and the agent is started again ten seconds after its last start, well
//! before the 20 s are up.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

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
async fn a_faulting_agent_held_off_is_started_again_at_once_when_its_vault_halts() {
    let executor = black_box::start(TRAPS_IN_RUN, black_box::Script::Approved).await;

    // The third fault, after which the agent is held off for 20 s.
    let started = Instant::now();
    while executor.lines_with("the strategy stopped") < 3 {
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "the agent did not fault three times:\n{}",
            executor.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        executor.lines_with("the strategy stopped"),
        3,
        "the agent was started again before its wait was over:\n{}",
        executor.said.lock().unwrap()
    );

    executor
        .node
        .halted_at
        .store(black_box::NOW, Ordering::SeqCst);
    let halted = Instant::now();
    while executor.lines_with("the strategy stopped") < 4 {
        assert!(
            halted.elapsed() < Duration::from_secs(14),
            "the agent was not started again for {:?} after its vault halted:\n{}",
            halted.elapsed(),
            executor.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
