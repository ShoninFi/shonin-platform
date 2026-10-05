//! A halt cuts a held-off restart short so that an agent is started again
//! to wind down in the grace. Until an agent has been handed the halting
//! notice, the halt cuts every such wait short, not only the first: a serve
//! that fails again before any agent hears of the halt leaves it as unheard
//! of as it was.
//!
//! Sequence: an open vault, key blessed and approved. The store's component
//! is not the one the bundle's manifest records, so every serve fails before
//! an agent runs, and the sweep holds the vault off 5 s, 10 s, 20 s, 40 s.
//! Once the first serve has failed the vault halts. A halt nobody has heard
//! of cuts a wait short no sooner than ten seconds in, so the 5 s and 10 s
//! waits run out; the halt cuts the 20 s wait short, the serve fails again,
//! and it cuts the 40 s wait that follows short too: two waits cut short by
//! the halt within a minute of it, where the waits alone would take more
//! than seventy seconds.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_halt_no_agent_was_told_of_cuts_every_wait_short_until_one_is() {
    let executor = black_box::start_recording(
        "the component the bundle's manifest records",
        "some other bytes in the store",
        black_box::Script::Approved,
        None,
    )
    .await;

    let started = Instant::now();
    while executor.lines_with("stopped serving this vault") == 0 {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the vault was never served:\n{}",
            executor.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    executor
        .node
        .halted_at
        .store(black_box::NOW, Ordering::SeqCst);
    let halted = Instant::now();
    let cut_short = || {
        executor
            .said
            .lock()
            .unwrap()
            .lines()
            .filter(|line| {
                line.contains("a held-off restart was cut short") && line.contains("Halted")
            })
            .count()
    };
    while cut_short() < 2 {
        assert!(
            halted.elapsed() < Duration::from_secs(60),
            "the vault halted and no agent was ever told, and {} of the waits since were cut \
             short in {:?}:\n{}",
            cut_short(),
            halted.elapsed(),
            executor.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
