//! A vault the factory lists that has never been read whole (the first read
//! after a restart, or a vault new to the indexer, with one of its reads
//! failing) is in the record as not read yet: an entry with its address and
//! what could be read of who it is, marked stale as of `NEVER_READ`, with no
//! figures and in no total. It counts toward its chain's staleness as any
//! stale vault does, so a chain whose only vault has never been read whole
//! is answered 503 by its health once `STALE_AFTER` has passed.
//!
//! Here one of the vault's reads (`fees()`) reverts from the start.

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chain::bindings::vault::IVault;
use indexer::STALE_AFTER;
use indexer::read::{NEVER_READ, never_read};
use stand_in::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chain_whose_only_vault_was_never_read_whole_is_not_answered_as_up() {
    let failing = Arc::new(AtomicBool::new(true));
    let flag = failing.clone();
    let indexer = an_indexer(
        Arc::new(move |request| {
            (flag.load(Ordering::SeqCst)
                && request["method"] == "eth_call"
                && input(request) == selector::<IVault::feesCall>())
            .then(|| Err(format!("{REVERTED}: a read the vault does not answer")))
        }),
        "never-read-whole",
    )
    .await;

    indexer.refresh().await.unwrap();
    let first = Instant::now();
    // Read again a little later, the vault still failing, as the indexer
    // does every couple of seconds.
    tokio::time::sleep(Duration::from_secs(3)).await;
    indexer.refresh().await.unwrap();
    {
        let snapshot = indexer.snapshot.read().await;
        assert_eq!(
            snapshot.strategies.len(),
            1,
            "a vault the factory lists, never read whole, is left out of the record"
        );
        let entry = &snapshot.strategies[0];
        assert_eq!(entry.vault.address, format!("{:?}", vault()));
        assert_eq!(entry.stale_as_of, Some(NEVER_READ));
        assert!(never_read(entry));
        assert!(
            entry.record.is_none(),
            "a vault never read whole has no record"
        );
        assert_eq!(entry.vault.value, 0.0);
        assert_eq!(snapshot.totals.vault_capital, 0.0);
        assert_eq!(snapshot.totals.trades, 0);
    }

    // Past STALE_AFTER since the first read, with its one vault never read
    // whole at any read: the chain has not been read.
    let later = first + STALE_AFTER + Duration::from_secs(1);
    let (up, said) = indexer.health_at("indexer", later);
    assert!(
        !up,
        "the chain's one vault has never been read whole, yet its health says up: {said}"
    );

    // Read whole at last, it is shown as read.
    failing.store(false, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await;
    assert_eq!(snapshot.strategies.len(), 1);
    assert!(snapshot.strategies[0].stale_as_of.is_none());
}
