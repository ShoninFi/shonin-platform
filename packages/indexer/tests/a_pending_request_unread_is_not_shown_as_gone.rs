//! A vault whose waiting requests can't be read is not shown, as fresh, with
//! none.
//!
//! A vault that can't be read whole keeps its last entry, marked with when
//! it was read (`stale_as_of`). A depositor's waiting request is part
//! of the vault like any other read: one that can't be read leaves the vault
//! as last read whole, its waiting requests with it, marked stale.
//!
//! Here a depositor's deposit is waiting (request 1, not done). A refresh
//! reads it. The next refresh's `requestAt(1)` fails (a node hiccup). The
//! snapshot still shows the waiting deposit, on a vault marked stale, rather
//! than a fresh vault with no waiting requests, as if the deposit had been
//! settled or never made.

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chain::bindings::vault::IVault;
use serde_json::json;
use stand_in::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pending_request_unread_is_not_shown_as_gone() {
    let failing = Arc::new(AtomicBool::new(false));
    let flag = failing.clone();
    let indexer = an_indexer(
        Arc::new(move |request| {
            if request["method"] != "eth_call" {
                return None;
            }
            let data = input(request);
            if data == selector::<IVault::requestCountCall>() {
                return Some(Ok(json!(format!("0x{:064x}", 1))));
            }
            if data.starts_with(&selector::<IVault::requestAtCall>()) {
                if flag.load(Ordering::SeqCst) {
                    return Some(Err("a node hiccup".to_string()));
                }
                // owner, kind (Deposit), earliest, done = false, amount, ref.
                return Some(Ok(json!(format!(
                    "0x{}{:064x}{:064x}{:064x}{:064x}{:064x}",
                    "0".repeat(24) + "00000000000000000000000000000000000000d0",
                    0,
                    1_790_000_100u64,
                    0,
                    25_000_000u64,
                    0
                ))));
            }
            None
        }),
        "a-pending-request",
    )
    .await;

    indexer.refresh().await.unwrap();
    {
        let snapshot = indexer.snapshot.read().await;
        assert_eq!(
            snapshot.strategies[0].vault.requests.len(),
            1,
            "the waiting deposit is read"
        );
    }

    failing.store(true, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await;
    let strategy = &snapshot.strategies[0];
    assert!(
        strategy.vault.requests.len() == 1 && strategy.stale_as_of.is_some(),
        "a vault whose waiting requests could not be read must keep the waiting deposit and \
         be marked stale: {} requests, stale_as_of {:?}",
        strategy.vault.requests.len(),
        strategy.stale_as_of
    );
}
