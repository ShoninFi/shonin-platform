//! A public RPC name fronts several nodes, and a list names several
//! endpoints: a head read that lands on a node a block behind the last one
//! is an ancestor on the same branch, not a replaced branch. Its refresh is
//! not published (the record stays as of the newer block, and its time is
//! not renewed), and nothing read is thrown away: the next refresh reads on
//! from where the record was, not from the deployment.
//!
//! Here: one vault, the platform deployed at block 1, the head at 1,000.
//! The second refresh reaches a node at 999; the third the head at 1,001.

mod stand_in;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use stand_in::*;

const HEAD: u64 = 1_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_head_from_a_node_a_block_behind_is_not_a_branch_change() {
    let log_reads: Arc<Mutex<Vec<u64>>> = Arc::default();
    let kept = log_reads.clone();
    let chain = Chain::at(HEAD);
    let indexer = an_indexer_on(
        chain.clone(),
        deployments(),
        Arc::new(move |request: &Value| {
            if request["method"] == "eth_getLogs" {
                kept.lock().unwrap().push(log_range(request).0);
            }
            None
        }),
        "a-block-behind",
    )
    .await;

    indexer.refresh().await.unwrap();
    let first = indexer.snapshot.read().await.chains[0].clone();
    assert_eq!(first.block, HEAD);
    assert!(
        log_reads
            .lock()
            .unwrap()
            .iter()
            .any(|from| *from <= DEPLOYED),
        "the first refresh reads the history from the deployment"
    );

    // A node a block behind answers.
    chain.head.store(HEAD - 1, Ordering::SeqCst);
    log_reads.lock().unwrap().clear();
    indexer.refresh().await.unwrap();
    let behind = indexer.snapshot.read().await.chains[0].clone();
    assert_eq!(
        (behind.block, behind.as_of),
        (first.block, first.as_of),
        "a head a block behind was published"
    );

    // The head moves on.
    chain.head.store(HEAD + 1, Ordering::SeqCst);
    log_reads.lock().unwrap().clear();
    indexer.refresh().await.unwrap();
    let after = indexer.snapshot.read().await.chains[0].clone();
    assert_eq!(after.block, HEAD + 1);
    let read_from: Vec<u64> = log_reads.lock().unwrap().clone();
    assert!(
        !read_from.is_empty() && read_from.iter().all(|from| *from > HEAD - 64),
        "after a head a block behind, logs were read again from blocks {read_from:?}"
    );
}
