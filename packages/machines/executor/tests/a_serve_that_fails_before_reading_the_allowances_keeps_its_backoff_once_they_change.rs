//! A vault whose serve keeps failing is held off ever longer (5 s doubling
//! to 5 min), and the sweep's wait is cut short only by what the next serve
//! will act on. A change of allowances is one such thing, judged against the
//! allowances in force when the wait began, so a serve that fails before it
//! reads the allowances does not leave a "change" for every sweep to find
//! again.
//!
//! Sequence: an open vault, key blessed and approved. The store's component
//! is not the one the bundle's manifest records, so the first serve reads the
//! allowances, refuses the component and ends with an error; the vault is
//! held off 5 s. Then the author raises the memory allowance, and the
//! bundle's manifest goes missing from the store, so every later serve fails
//! before it reads the allowances. With the backoff kept, the 30 s after the
//! change see at most three more serves.

mod black_box;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_serve_that_fails_before_reading_the_allowances_keeps_its_backoff_once_they_change() {
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
    assert!(
        executor.lines_with("not the vault's bundle") >= 1,
        "the first serve did not get as far as the component:\n{}",
        executor.said.lock().unwrap()
    );

    // The author raises the allowances; the manifest is gone from the store.
    executor.node.raised.store(true, Ordering::SeqCst);
    for entry in std::fs::read_dir(executor.dir.join("store")).unwrap() {
        let entry = entry.unwrap();
        if entry.path().join("manifest.json").exists() {
            std::fs::remove_file(entry.path().join("manifest.json")).unwrap();
        }
    }
    let before = executor.lines_with("stopped serving this vault");
    tokio::time::sleep(Duration::from_secs(30)).await;
    let serves = executor.lines_with("stopped serving this vault") - before;
    let cut_short = executor.lines_with("a held-off restart was cut short");
    drop(executor);
    assert!(
        serves <= 3,
        "a serve failing before it reads the allowances was tried {serves} times in 30 s \
         ({cut_short} waits cut short for an allowance change it never ran under): the \
         backoff is gone"
    );
}
