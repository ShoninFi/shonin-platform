//! An agent held off after faulting is started again at once for a halt it
//! was never told of, not only for one that lands during the wait: a halt
//! that lands while the agent runs, which it faults before reading, is
//! already there when the wait begins, and the agent must not sit out its
//! backoff (up to five minutes) of the halt's grace untold.
//!
//! Sequence: an open vault; its agent busies itself for a few seconds after
//! `run` is called (held to the vault's fuel rate) and then traps, so it is held off 5 s, 10 s, 20 s. During
//! its fourth run the vault halts. The agent traps without having read the
//! notice (it never reads the vault's stream), and it is started again at
//! once rather than after the 40 s that would follow.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// A component that loops for a few seconds once `run` is called, and traps:
/// eight fuel a turn, 7.5 million turns, which at the stand-in vault's
/// modest fuel rate (ten million a second, a second of it saved up before
/// each run) lasts about five seconds.
const BUSY_THEN_TRAPS: &str = r#"(component
    (core module $m
        (memory (export "memory") 1)
        (func (export "run") (result i32)
            (local $i i64)
            (loop $l
                (local.set $i (i64.add (local.get $i) (i64.const 1)))
                (br_if $l (i64.lt_u (local.get $i) (i64.const 7500000))))
            unreachable))
    (core instance $i (instantiate $m))
    (func $run async (result (result (error string)))
        (canon lift (core func $i "run") (memory (core memory $i "memory"))))
    (export "run" (func $run)))"#;

async fn faults_reach(executor: &black_box::Running, count: usize, within: Duration) -> Instant {
    let started = Instant::now();
    while executor.lines_with("the strategy stopped") < count {
        assert!(
            started.elapsed() < within,
            "fault {count} did not come within {within:?}:\n{}",
            executor.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Instant::now()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_halt_the_agent_was_never_told_of_cuts_its_wait_short() {
    let executor = black_box::start(BUSY_THEN_TRAPS, black_box::Script::Approved).await;

    let first = faults_reach(&executor, 1, Duration::from_secs(120)).await;
    let second = faults_reach(&executor, 2, Duration::from_secs(120)).await;
    // How long one run lasts: the gap less the 5 s wait.
    let run = second
        .duration_since(first)
        .saturating_sub(Duration::from_secs(5));
    assert!(
        run >= Duration::from_secs(3),
        "a run lasted only {run:?}; the test needs it to last a few seconds"
    );
    let third = faults_reach(&executor, 3, Duration::from_secs(120)).await;

    // The fourth run starts 20 s after the third fault; halt half-way in.
    tokio::time::sleep(Duration::from_secs(20) + run / 2).await;
    assert_eq!(executor.lines_with("the strategy stopped"), 3);
    executor
        .node
        .halted_at
        .store(black_box::NOW, Ordering::SeqCst);
    let fourth = faults_reach(&executor, 4, Duration::from_secs(60)).await;
    assert!(fourth.duration_since(third) > Duration::from_secs(20));

    // Started again at once, it faults again one run later.
    let fifth = faults_reach(&executor, 5, Duration::from_secs(120)).await;
    let waited = fifth.duration_since(fourth).saturating_sub(run);
    assert!(
        waited < Duration::from_secs(10),
        "the vault halted while the agent ran, the agent was never told, and it was \
         held off {waited:?} of its grace:\n{}",
        executor.said.lock().unwrap()
    );
}
