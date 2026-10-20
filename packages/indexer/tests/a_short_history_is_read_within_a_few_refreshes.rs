//! A chain's record starts at the platform's deployment. Catching up a
//! history of a couple of thousand quiet blocks (one log page) must take a
//! few refreshes with the shipped settings, not a long stretch of refreshes
//! during which the chain answers as not read; a testnet carries tens of
//! thousands of blocks a day. Nor is any block's header read on the way
//! that no log or page end names.
//!
//! Here: a node whose blocks carry no logs at all, the platform deployed
//! 2,000 blocks below the head, one vault, the checked-in settings.

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chain::{ChainKey, Served};
use indexer::Indexer;
use serde_json::Value;
use stand_in::*;

const DEPLOYED: u64 = 1_000_000;
const LATEST: u64 = DEPLOYED + 1_999;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_short_history_is_read_within_a_few_refreshes() {
    let header_reads = Arc::new(AtomicU64::new(0));
    let counted = header_reads.clone();
    let node = a_node_on(
        Chain::at(LATEST),
        Arc::new(move |request: &Value| {
            if request["method"] == "eth_getBlockByNumber"
                && request["params"][0]
                    .as_str()
                    .is_some_and(|tag| tag.starts_with("0x"))
            {
                counted.fetch_add(1, Ordering::SeqCst);
            }
            None
        }),
    )
    .await;
    let store = std::env::temp_dir().join(format!(
        "shonin-indexer-short-history-{}",
        std::process::id()
    ));
    // The checked-in settings, as a service connects with them.
    let indexer = Arc::new(Indexer::new(vec![shared::Chain::Base], store, None));
    indexer
        .add(Served {
            vault_chain: shared::Chain::Base,
            key: "evm:31337".parse::<ChainKey>().unwrap(),
            client: client(&node).await,
            deployments: deployments_from(DEPLOYED),
            native_usd: None,
            public_rpc: String::new(),
        })
        .await;

    let mut refreshes = 0;
    tokio::time::timeout(Duration::from_secs(120), async {
        for _ in 0..8 {
            indexer.refresh().await.unwrap();
            refreshes += 1;
            let snapshot = indexer.snapshot.read().await;
            if snapshot
                .chain(shared::Chain::Base)
                .is_some_and(|info| info.block == LATEST && !info.catching_up)
                && snapshot.strategies.len() == 1
                && indexer.health_at("indexer", std::time::Instant::now()).0
            {
                break;
            }
        }
    })
    .await
    .expect("eight refreshes end");
    let snapshot = indexer.snapshot.read().await.clone();
    let block = snapshot
        .chain(shared::Chain::Base)
        .map(|info| info.block)
        .unwrap_or(0);
    let (up, said) = indexer.health_at("indexer", std::time::Instant::now());
    let headers = header_reads.load(Ordering::SeqCst);
    assert!(
        block == LATEST && snapshot.strategies.len() == 1 && up,
        "after {refreshes} refreshes the chain is read to block {block} (head {LATEST}), \
         vaults in the record: {}, health: {said:?}; headers read by number: {headers}",
        snapshot.strategies.len(),
    );
    assert!(
        refreshes <= 2,
        "a history of one page took {refreshes} refreshes"
    );
    // The endpoint's check (a handful), each page's end, the readings'
    // search: never one per block.
    assert!(
        headers < 100,
        "{headers} headers were read by number for 2,000 quiet blocks"
    );
}
