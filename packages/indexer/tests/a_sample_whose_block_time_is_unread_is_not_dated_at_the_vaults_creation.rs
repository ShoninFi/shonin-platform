//! A reading whose block's time could not be read is not taken then, and is
//! asked for again at the next refresh: never dated by a guess (the vault's
//! creation, say), which would put a point at today's price in the middle of
//! the record, the series going back in time, and the record's drawdown and
//! monthly returns computed over it.
//!
//! Here: hour-long blocks (the local devnet's clock), a vault sixty days old,
//! and a node that fails one past block's header the first time it is asked
//! for it (a provider's rate limit).

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use stand_in::*;

const DEPLOYED: u64 = 1_200_000;
const HEAD: u64 = DEPLOYED + 12 * 24 * 5;
const FAILS: u64 = DEPLOYED + 12 * 24 * 3;
const T0: u64 = 1_790_000_000;

/// Whether the failing block's header has been asked for yet.
static FAILED: AtomicBool = AtomicBool::new(false);

fn time_of(block: u64) -> u64 {
    T0 + 3_600 * block.saturating_sub(DEPLOYED)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sample_whose_block_time_is_unread_is_not_dated_at_the_vaults_creation() {
    let indexer = an_indexer_on(
        Chain::dated(HEAD, time_of),
        deployments_from(DEPLOYED),
        Arc::new(|request: &Value| match request["method"].as_str() {
            Some("eth_getBlockByNumber")
                if requested_block(request, HEAD) == FAILS
                    && !FAILED.swap(true, Ordering::SeqCst) =>
            {
                Some(Err("rate limited".to_string()))
            }
            Some("eth_call") => {
                let data = input(request);
                if data == selector::<chain::bindings::vault::IVault::createdAtCall>() {
                    Some(Ok(json!(format!("0x{T0:064x}"))))
                } else if data == selector::<chain::bindings::vault::IVault::sharePriceCall>() {
                    // $1.00 a share, as the asset counts it.
                    Some(Ok(json!(format!("0x{:064x}", 1_000_000u64))))
                } else {
                    None
                }
            }
            _ => None,
        }),
        "misdated",
    )
    .await;
    // Enough refreshes to read every hourly block since the vault's creation.
    for _ in 0..40 {
        indexer.refresh().await.unwrap();
    }

    assert!(
        FAILED.load(Ordering::SeqCst),
        "the failing header was asked for"
    );
    let snapshot = indexer.snapshot.read().await.clone();
    let strategy = snapshot.strategies.first().expect("the vault is read");
    let record = strategy.record.as_ref().expect("a record");
    let dates: Vec<_> = record
        .value_per_share
        .iter()
        .map(|point| point.date)
        .collect();
    assert!(
        dates.windows(2).all(|pair| pair[0] < pair[1]),
        "the record's days go back in time: {dates:?}"
    );
    // Every day from the vault's creation to the head's, the one whose
    // header failed included.
    let days = time_of(HEAD) / 86_400 - T0 / 86_400 + 1;
    assert_eq!(dates.len() as u64, days, "the record's days: {dates:?}");
}
