//! Every request still waiting to be priced is shown, however many requests
//! came after it. The indexer reads a vault's requests from the first not
//! known to be done to the latest, a bounded page at each refresh; until it
//! has read them all the vault is not read whole and is marked stale, never
//! shown with part of its waiting requests as if they were all.
//!
//! Here a vault has 600 requests; request 5 is still waiting and every other
//! is done.

mod stand_in;

use std::sync::Arc;

use chain::bindings::vault::IVault;
use serde_json::json;
use stand_in::*;

const REQUESTS: u64 = 600;
const WAITING: u64 = 5;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_waiting_request_is_shown_however_many_came_after_it() {
    let indexer = an_indexer(
        Arc::new(move |request| {
            if request["method"] != "eth_call" {
                return None;
            }
            let data = input(request);
            if data == selector::<IVault::requestCountCall>() {
                return Some(Ok(json!(format!("0x{REQUESTS:064x}"))));
            }
            let at = selector::<IVault::requestAtCall>();
            if let Some(id) = data.strip_prefix(&at) {
                let id = u64::from_str_radix(id.trim_start_matches('0'), 16).unwrap_or(0);
                let done = u64::from(id != WAITING);
                // owner, kind (Deposit), earliest, done, amount, ref.
                return Some(Ok(json!(format!(
                    "0x{}{:064x}{:064x}{:064x}{:064x}{:064x}",
                    "0".repeat(24) + "00000000000000000000000000000000000000d0",
                    0,
                    1_790_000_100u64,
                    done,
                    25_000_000u64,
                    0
                ))));
            }
            None
        }),
        "every-waiting-request",
    )
    .await;

    // The first refresh can't read 600 requests: the vault isn't read whole.
    indexer.refresh().await.unwrap();
    assert!(
        indexer.snapshot.read().await.strategies[0]
            .stale_as_of
            .is_some(),
        "a vault whose requests were read only in part is shown as read whole"
    );

    // Each refresh reads on from where the last stopped, and a few see them
    // all.
    let mut refreshes = 1;
    while indexer.snapshot.read().await.strategies[0]
        .stale_as_of
        .is_some()
    {
        assert!(refreshes < 5, "the vault's requests are never all read");
        indexer.refresh().await.unwrap();
        refreshes += 1;
    }
    let snapshot = indexer.snapshot.read().await;
    let ids: Vec<u64> = snapshot.strategies[0]
        .vault
        .requests
        .iter()
        .map(|request| request.id)
        .collect();
    assert_eq!(
        ids,
        vec![WAITING],
        "the waiting request, {} requests before the latest, is not shown",
        REQUESTS - WAITING
    );

    // Read whole, the next refresh reads only what is not known done.
    drop(snapshot);
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await;
    assert!(snapshot.strategies[0].stale_as_of.is_none());
    assert_eq!(snapshot.strategies[0].vault.requests.len(), 1);
}
