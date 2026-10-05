//! A chain whose every vault is shown as last read whole, none of them read
//! whole now (a node that answers the chain's head and none of its vaults'
//! reads), is not a chain being read: once that has lasted past
//! `STALE_AFTER`, its health says so and answers 503, as it does for a chain
//! not read at all.

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chain::bindings::vault::IVault;
use indexer::STALE_AFTER;
use stand_in::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chain_whose_every_vault_is_stale_is_not_answered_as_up() {
    let failing = Arc::new(AtomicBool::new(false));
    let flag = failing.clone();
    let indexer = an_indexer(
        Arc::new(move |request| {
            (flag.load(Ordering::SeqCst)
                && request["method"] == "eth_call"
                && input(request) == selector::<IVault::totalValueCall>())
            .then(|| Err("a node hiccup".to_string()))
        }),
        "every-vault-stale",
    )
    .await;

    indexer.refresh().await.unwrap();
    let read_whole = Instant::now();
    let (up, said) = indexer.health_at("indexer", read_whole);
    assert!(up, "{said}");

    // Read again a little later, every vault failing.
    tokio::time::sleep(Duration::from_secs(3)).await;
    failing.store(true, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    {
        let snapshot = indexer.snapshot.read().await;
        assert!(
            snapshot.strategies[0].stale_as_of.is_some(),
            "the vault is shown as last read whole"
        );
    }
    let (up, said) = indexer.health_at("indexer", Instant::now());
    assert!(up, "a moment's stale vaults are no outage: {said}");
    // Past STALE_AFTER since the chain was last read whole, and not since it
    // was last asked.
    let later = read_whole + STALE_AFTER + Duration::from_secs(1);
    let (up, said) = indexer.health_at("indexer", later);
    assert!(
        !up,
        "every vault on the chain has been stale past STALE_AFTER and its health says up: {said}"
    );
    assert!(said.contains("every vault stale"), "{said}");
}
