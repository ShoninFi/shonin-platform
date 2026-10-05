//! A halt that no agent has been handed cuts every held-off restart short,
//! so that an agent can be told of it and wind down. An agent that faults
//! before it ever reads its vault stream (it traps in `run`) is never handed
//! the notice, so the halt goes on cutting its waits short for the whole
//! grace; it does so no sooner than ten seconds after the agent was last
//! started, rather than at every look (two seconds) with no backoff at all.
//!
//! An open vault, key blessed and approved, tank full; its component traps
//! as soon as `run` is called. After the first fault the vault halts (the
//! stand-in's clock never reaches the end of the 600 s grace). Over the next
//! 40 s the agent is started about four times, its waits cut short by the
//! halt, where the backoff alone (20 s, then 40 s) would start it once or
//! twice.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::Duration;

const TRAPS_IN_RUN: &str = r#"(component
    (core module $m
        (memory (export "memory") 1)
        (func (export "run") (result i32) unreachable))
    (core instance $i (instantiate $m))
    (func $run async (result (result (error string)))
        (canon lift (core func $i "run") (memory (core memory $i "memory"))))
    (export "run" (func $run)))"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_halt_nobody_has_heard_of_starts_a_faulting_agent_no_more_than_every_ten_seconds() {
    let executor = black_box::start(TRAPS_IN_RUN, black_box::Script::Approved).await;

    let started = std::time::Instant::now();
    while executor.lines_with("the strategy stopped") == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the agent was never started:\n{}",
            executor.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    executor
        .node
        .halted_at
        .store(black_box::NOW, Ordering::SeqCst);
    let before = executor.lines_with("the strategy stopped");
    tokio::time::sleep(Duration::from_secs(40)).await;
    let starts = executor.lines_with("the strategy stopped") - before;
    let woken = executor.lines_with("starting the agent again to wind down");
    let said = executor.said.lock().unwrap().clone();
    drop(executor);
    assert!(
        starts <= 5,
        "after the halt an agent that never reads its vault stream was started {starts} \
         times in 40 s ({woken} waits cut short by the same halt): more often than every \
         ten seconds:\n{said}"
    );
    assert!(
        woken >= 2,
        "a halt nobody has heard of cut only {woken} waits short in 40 s:\n{said}"
    );
}
