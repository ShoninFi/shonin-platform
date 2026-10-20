//! A reading the vault doesn't answer (a node failing a read now and then)
//! is asked for again at the next refresh, not passed over: passed over, the
//! record has no reading for that hour, for good.
//!
//! Here: a chain with 2 s blocks, the vault created at block 1, three hours
//! of chain time; the node fails the first two value reads asked of a past
//! block.

mod stand_in;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use stand_in::*;

const DEPLOYED: u64 = 1;
const HEAD: u64 = 1 + 3 * 1_800;
const GENESIS_TIME: u64 = 1_700_000_000;

/// What the node made of the value reads asked of past blocks: the blocks it
/// failed, and those it answered.
#[derive(Default)]
struct Reads {
    failed: BTreeSet<u64>,
    answered: BTreeSet<u64>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reading_the_vault_does_not_answer_is_asked_again() {
    let reads = Arc::new(Mutex::new(Reads::default()));
    let kept = reads.clone();
    let indexer = an_indexer_on(
        Chain::dated(HEAD, |number| GENESIS_TIME + 2 * number),
        deployments_from(DEPLOYED),
        Arc::new(move |request: &Value| {
            let reading = request["method"] == "eth_call"
                && input(request)
                    .starts_with(&selector::<chain::bindings::vault::IVault::sharePriceCall>());
            let at = requested_block(request, HEAD);
            if reading && at != HEAD {
                let mut reads = kept.lock().unwrap();
                if reads.failed.len() < 2 && !reads.failed.contains(&at) {
                    reads.failed.insert(at);
                    return Some(Err("header not found".to_string()));
                }
                reads.answered.insert(at);
            }
            None
        }),
        "unanswered-reading",
    )
    .await;
    for _ in 0..4 {
        indexer.refresh().await.unwrap();
    }
    let reads = reads.lock().unwrap();
    assert!(!reads.failed.is_empty(), "no read was failed");
    let lost: Vec<u64> = reads.failed.difference(&reads.answered).copied().collect();
    assert!(
        lost.is_empty(),
        "the readings at blocks {lost:?} failed once and were never asked for again (answered: {:?})",
        reads.answered
    );
}
