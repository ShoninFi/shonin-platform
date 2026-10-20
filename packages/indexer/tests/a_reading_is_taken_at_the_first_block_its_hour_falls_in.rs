//! Each of a vault's readings is taken at the first block its hour falls
//! in, not at whichever block a guess from the chain's average pace lands on
//! once that block's time is due: on a chain whose pace changes (one quiet
//! for days, then busy) such a guess overshoots, and days of the record
//! would have no reading.
//!
//! Here: the vault is created at block 1; blocks 1..=1,000 come one every
//! 864 s (ten quiet days), then one a second. The average pace from block 1
//! to the head puts the hour after block 1 near block 378, nearly four days
//! on.

mod stand_in;

use std::sync::Arc;

use serde_json::Value;
use stand_in::*;

const DEPLOYED: u64 = 1;
const QUIET: u64 = 1_000;
const QUIET_GAP: u64 = 864;
const GENESIS_TIME: u64 = 1_700_000_000;

fn time_of(block: u64) -> u64 {
    if block <= QUIET {
        GENESIS_TIME + QUIET_GAP * block
    } else {
        GENESIS_TIME + QUIET_GAP * QUIET + (block - QUIET)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_reading_is_the_first_block_its_hour_falls_in() {
    let head = QUIET + 100_000;
    let samples = Arc::new(std::sync::Mutex::new(Vec::new()));
    let kept = samples.clone();
    // Keeps the block of each `sharePrice` read asked of a past block: one
    // per reading.
    let indexer = an_indexer_on(
        Chain::dated(head, time_of),
        deployments_from(DEPLOYED),
        Arc::new(move |request: &Value| {
            if request["method"] == "eth_call"
                && input(request)
                    .starts_with(&selector::<chain::bindings::vault::IVault::sharePriceCall>())
            {
                let at = requested_block(request, head);
                if at != head {
                    kept.lock().unwrap().push(at);
                }
            }
            None
        }),
        "first-block-of-the-hour",
    )
    .await;
    for _ in 0..3 {
        indexer.refresh().await.unwrap();
    }
    let mut blocks = samples.lock().unwrap().clone();
    blocks.dedup();
    assert!(blocks.len() >= 2, "readings at {blocks:?}");
    for pair in blocks.windows(2) {
        let gap = time_of(pair[1]) - time_of(pair[0]);
        assert!(
            gap <= 3_600 + QUIET_GAP,
            "readings at blocks {} and {} are {:.1} days apart; an hourly record has a reading in every hour's first block",
            pair[0],
            pair[1],
            gap as f64 / 86_400.0
        );
    }
}
