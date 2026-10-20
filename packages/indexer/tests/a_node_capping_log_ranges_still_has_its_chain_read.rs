//! Most providers cap how many blocks one `eth_getLogs` may span (10,000
//! is common; some cap at 1,000 or 2,000). Every log the indexer reads (the
//! feed's registrations and image proposals, the vaults' activations, each
//! vault's events, fills and blessings) is read a page at a time, a page the
//! node refuses halved until it is taken, so such a node still has its
//! chain read: the record is written, the vault is in the snapshot, and the
//! chain answers as up.
//!
//! Here: a node refusing a log request spanning more than 1,000 blocks
//! (narrower than a page), the platform deployed 20,000 blocks ago, one
//! vault.

mod stand_in;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use stand_in::*;

const DEPLOYED: u64 = 1_000_000;
const HEAD: u64 = DEPLOYED + 20_000;
const RANGE_CAP: u64 = 1_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_capping_log_ranges_still_has_its_chain_read() {
    let indexer = an_indexer_on(
        Chain::at(HEAD),
        deployments_from(DEPLOYED),
        Arc::new(|request: &Value| {
            if request["method"] != "eth_getLogs" {
                return None;
            }
            let (from, to) = log_range(request);
            Some(if to.saturating_sub(from) >= RANGE_CAP {
                Err("query exceeds max block range 1000".to_string())
            } else {
                Ok(json!([]))
            })
        }),
        "log-cap",
    )
    .await;

    tokio::time::timeout(Duration::from_secs(30), indexer.refresh())
        .await
        .expect("a refresh ends")
        .unwrap();
    let snapshot = indexer.snapshot.read().await.clone();
    let block = snapshot
        .chain(shared::Chain::Base)
        .map(|info| info.block)
        .unwrap_or(0);
    let vaults = snapshot.strategies.len();
    let (up, said) = indexer.health_at("indexer", std::time::Instant::now());
    assert!(
        block == HEAD && vaults == 1 && up,
        "chain read to block {block} (head {HEAD}); vaults in the record: {vaults}; \
         health: {said:?}"
    );
}
