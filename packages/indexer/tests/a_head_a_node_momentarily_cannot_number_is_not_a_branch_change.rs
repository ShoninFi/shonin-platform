//! A node that momentarily cannot answer a block by its number (a node
//! behind the one that gave the head, a rate limit, a timeout) has not
//! changed the branch: the refresh that needed it is simply not published,
//! and nothing read is thrown away. Treating the failure as a branch change
//! would throw away every log, reading and cache and read the chain again
//! from the deployment.
//!
//! Here: one vault, the platform deployed at block 1, the head at 1,000.
//! During the second refresh the head is 1,001 and the node answers block
//! 1,001 by its number as missing, once. The third refresh, at 1,002, reads
//! on from where the record was.

mod stand_in;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use stand_in::*;

const HEAD: u64 = 1_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_head_a_node_momentarily_cannot_number_is_not_a_branch_change() {
    let log_reads: Arc<Mutex<Vec<u64>>> = Arc::default();
    let missing = Arc::new(AtomicBool::new(false));
    let missed = Arc::new(AtomicU64::new(0));
    let chain = Chain::at(HEAD);
    let indexer = an_indexer_on(
        chain.clone(),
        deployments(),
        {
            let log_reads = log_reads.clone();
            let missing = missing.clone();
            let missed = missed.clone();
            Arc::new(move |request: &Value| match request["method"].as_str() {
                Some("eth_getLogs") => {
                    log_reads.lock().unwrap().push(log_range(request).0);
                    None
                }
                Some("eth_getBlockByNumber")
                    if request["params"][0].as_str() == Some(&format!("{:#x}", HEAD + 1))
                        && missing.swap(false, Ordering::SeqCst) =>
                {
                    missed.fetch_add(1, Ordering::SeqCst);
                    Some(Ok(Value::Null))
                }
                _ => None,
            })
        },
        "momentarily-unnumbered",
    )
    .await;

    indexer.refresh().await.unwrap();
    assert_eq!(indexer.snapshot.read().await.chains[0].block, HEAD);

    chain.head.store(HEAD + 1, Ordering::SeqCst);
    missing.store(true, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    assert_eq!(
        missed.load(Ordering::SeqCst),
        1,
        "the block was asked for by its number and answered as missing once"
    );

    chain.head.store(HEAD + 2, Ordering::SeqCst);
    log_reads.lock().unwrap().clear();
    indexer.refresh().await.unwrap();
    assert_eq!(indexer.snapshot.read().await.chains[0].block, HEAD + 2);
    let read_from: Vec<u64> = log_reads.lock().unwrap().clone();
    assert!(
        !read_from.is_empty() && read_from.iter().all(|from| *from > HEAD - 64),
        "a momentary failure to number a block must not rebuild the record from the \
         deployment; logs were read again from blocks {read_from:?}"
    );
}
