//! What the record serves is dated by the block it was read as of, and is
//! never passed off as current when it is not:
//!
//! - a refresh that cannot be read whole (an endpoint behind, or one that
//!   answers no observation) publishes nothing: the chain keeps the block
//!   and time it was last read as of, and its health turns down once the
//!   record has gone unrenewed for `record_stale_ms`;
//! - a history not yet read up to the head (after a start) is published
//!   marked as catching up, dated by the last block read whole, not by the
//!   head, and its health is not renewed meanwhile.

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use stand_in::*;

const HEAD: u64 = 1_000;

fn time_of(block: u64) -> u64 {
    TIME + 2 * block
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refresh_not_read_whole_publishes_nothing_and_health_turns_down() {
    let failing = Arc::new(AtomicBool::new(false));
    let flag = failing.clone();
    let chain = Chain::dated(HEAD, time_of);
    let indexer = an_indexer_on(
        chain.clone(),
        deployments(),
        Arc::new(move |request: &Value| {
            // The endpoint stops answering calls: no observation.
            (flag.load(Ordering::SeqCst) && request["method"] == "eth_call")
                .then(|| Err("the node is overloaded".to_string()))
        }),
        "never-current-failing",
    )
    .await;
    indexer.refresh().await.unwrap();
    let read = Instant::now();
    let first = indexer.snapshot.read().await.chains[0].clone();
    assert_eq!(first.block, HEAD);
    assert_eq!(
        first.as_of,
        Some(indexer::read::moment(time_of(HEAD))),
        "dated by its block, on the chain's own clock"
    );

    // A little later the chain has moved on; the endpoint answers its head
    // and no call. (Renewed then, the record would be up a second past
    // `record_stale_ms` from its first read.)
    tokio::time::sleep(Duration::from_secs(2)).await;
    chain.head.store(HEAD + 10, Ordering::SeqCst);
    failing.store(true, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    let after = indexer.snapshot.read().await.chains[0].clone();
    assert_eq!(
        (after.block, after.as_of),
        (first.block, first.as_of),
        "a refresh that read no observation was published"
    );
    let stale = read + indexer::STALE_AFTER + Duration::from_secs(1);
    let (up, said) = indexer.health_at("indexer", stale);
    assert!(
        !up,
        "unrenewed past record_stale_ms, health says up: {said}"
    );

    // Behind: an endpoint whose head is below the record's.
    failing.store(false, Ordering::SeqCst);
    chain.head.store(HEAD - 5, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    let behind = indexer.snapshot.read().await.chains[0].clone();
    assert_eq!(
        (behind.block, behind.as_of),
        (first.block, first.as_of),
        "a head behind the record was published"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_history_still_being_read_is_published_as_catching_up_and_not_as_current() {
    const DEPLOYED: u64 = 1;
    // Three refreshes' worth of pages at the checked-in settings.
    const LONG: u64 = 120_000;
    let chain = Chain::dated(LONG, time_of);
    let indexer = an_indexer_on(
        chain.clone(),
        deployments_from(DEPLOYED),
        Arc::new(|_| None),
        "never-current-catching-up",
    )
    .await;
    indexer.refresh().await.unwrap();
    let info = indexer.snapshot.read().await.chains[0].clone();
    assert!(info.catching_up, "{info:?}");
    assert!(
        info.block < LONG,
        "a history read part of the way is dated by its head: {info:?}"
    );
    assert_eq!(
        info.as_of,
        Some(indexer::read::moment(time_of(info.block))),
        "dated by the last block read whole"
    );
    let later = Instant::now() + indexer::STALE_AFTER + Duration::from_secs(1);
    let (up, said) = indexer.health_at("indexer", later);
    assert!(!up, "a record catching up renewed its health: {said}");
    assert!(said.contains("catching up"), "{said}");

    for _ in 0..4 {
        indexer.refresh().await.unwrap();
    }
    let info = indexer.snapshot.read().await.chains[0].clone();
    assert!(!info.catching_up && info.block == LONG, "{info:?}");
    let (up, said) = indexer.health_at("indexer", Instant::now());
    assert!(up, "{said}");
}
