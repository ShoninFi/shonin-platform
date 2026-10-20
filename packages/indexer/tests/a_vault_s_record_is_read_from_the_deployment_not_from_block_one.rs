//! A vault's value-per-share record is sampled from the block the vault was
//! created in (its factory's `Activated` log, or the platform's deployment
//! when that can't be read), never from block 1, and every block asked
//! counts towards a refresh's cap whether or not it answers.
//!
//! Sampled from block 1, a vault's first refresh on a chain whose head is far
//! past it (every testnet and mainnet) makes one failing `eth_call` per twelve
//! blocks of the chain's whole history, and the chain's part of the record
//! is not written until it ends.
//!
//! Here: the platform was deployed at block 1,999,000 of a 2,000,000-block
//! chain, and the one vault was created there; the node answers no logs. A
//! refresh is given 20 s.

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use stand_in::*;

const HEAD: u64 = 2_000_000;
const DEPLOYED: u64 = 1_999_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_s_record_is_read_from_the_deployment_not_from_block_one() {
    let before_deployment = Arc::new(AtomicU64::new(0));
    let counted = before_deployment.clone();
    // The contracts exist from `DEPLOYED` on: a read at an earlier block
    // finds no code and returns nothing.
    let indexer = an_indexer_on(
        Chain::at(HEAD),
        deployments_from(DEPLOYED),
        Arc::new(move |request: &Value| {
            (request["method"] == "eth_call" && requested_block(request, HEAD) < DEPLOYED).then(
                || {
                    counted.fetch_add(1, Ordering::SeqCst);
                    Ok(json!("0x"))
                },
            )
        }),
        "sample-from-deployment",
    )
    .await;

    let finished = tokio::time::timeout(Duration::from_secs(20), indexer.refresh())
        .await
        .is_ok();
    let asked_before = before_deployment.load(Ordering::SeqCst);
    // In the record, and read whole: not merely listed as not read yet.
    let in_record = indexer
        .snapshot
        .read()
        .await
        .strategies
        .iter()
        .any(|strategy| strategy.stale_as_of.is_none());
    assert!(
        finished && in_record && asked_before == 0,
        "refresh finished in 20 s: {finished}; vault in the record: {in_record}; \
         reads asked of blocks before the deployment so far: {asked_before} \
         (of about {} the first refresh would make)",
        DEPLOYED / 12
    );
}
