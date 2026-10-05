//! A vault's events are read from the block its key's stake was begun at
//! for the take-on that happened. A stake begun for a take-on that then
//! didn't happen (the vault found taken over by another executor, or the
//! station refusing this key) is dropped, so when the vault is taken on
//! again, perhaps days later, its watcher doesn't read every block since
//! and hand the new agent, as if new, events long since dealt with.
//!
//! Sequence: nobody approved at the station; the executor's first sweep
//! begins the key's stake at block 0x10. Before the next sweep another
//! executor's blessed key is approved: the vault is let be. Six seconds on,
//! this executor's key is approved again and the head is at 0x5000. The
//! vault is taken on; its watcher's first log request is from 0x5000 on.

mod black_box;

use std::time::Duration;

/// A component that runs until it is stopped.
const RUNS: &str = r#"(component
    (core module $m
        (memory (export "memory") 1)
        (func (export "run") (result i32) (loop $on (br $on)) (i32.const 0)))
    (core instance $i (instantiate $m))
    (func $run async (result (result (error string)))
        (canon lift (core func $i "run") (memory (core memory $i "memory"))))
    (export "run" (func $run)))"#;

const LATER_HEAD: u64 = 0x5000;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_vault_taken_back_reads_its_events_from_when_it_was_taken_back() {
    let executor = black_box::start(
        RUNS,
        black_box::Script::TakenOverThenBack {
            replaced_for: Duration::from_secs(6),
            later_head: LATER_HEAD,
        },
    )
    .await;

    let started = std::time::Instant::now();
    let first = loop {
        if let Some(from) = executor.node.logs_from.lock().unwrap().first().copied() {
            break from;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the vault was never taken on and watched:\n{}",
            executor.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let said = executor.said.lock().unwrap().clone();
    drop(executor);
    assert!(
        said.contains("another executor's key holds the approval"),
        "the vault was never seen taken over, so this proves nothing:\n{said}"
    );
    assert!(
        first >= LATER_HEAD,
        "taken on at block {LATER_HEAD:#x}, its events were read from block {first:#x}, \
         the head when a stake was begun for an earlier take-on that never happened"
    );
}
