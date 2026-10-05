//! A vault one read fails for, once, stays in the record. A chain that
//! can't be read keeps its last part; a vault that can't be read whole (any
//! one of its twenty-odd reads failing) keeps its last entry, marked with
//! when it was read, rather than vanishing from the snapshot, the totals and
//! every holder's portfolio until a refresh reads it whole again.
//!
//! Here: the vault is read whole once; then its `fees()` read fails once.

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chain::bindings::vault::IVault;
use stand_in::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_one_read_fails_for_stays_in_the_record() {
    let failing = Arc::new(AtomicBool::new(false));
    let flag = failing.clone();
    let indexer = an_indexer(
        Arc::new(move |request| {
            (flag.load(Ordering::SeqCst)
                && request["method"] == "eth_call"
                && input(request) == selector::<IVault::feesCall>())
            .then(|| Err("a node hiccup".to_string()))
        }),
        "one-read-fails",
    )
    .await;

    indexer.refresh().await.unwrap();
    assert_eq!(indexer.snapshot.read().await.strategies.len(), 1);

    failing.store(true, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await;
    let shown = snapshot.strategies.len();
    assert_eq!(
        shown, 1,
        "one failed read of a vault took it out of the record: {shown} vaults shown"
    );
    assert!(
        snapshot.strategies[0].stale_as_of.is_some(),
        "a vault kept as last read is not marked with when that was"
    );
    drop(snapshot);

    failing.store(false, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await;
    assert!(
        snapshot.strategies[0].stale_as_of.is_none(),
        "a vault read whole again is still marked as kept"
    );
}
