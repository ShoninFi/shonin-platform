//! The authors are one part of a chain's read: when they can't be read (a
//! profile read that fails), the chain's vaults are still read and put in
//! place, and the authors are kept as last read.

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chain::bindings::profiles::IAuthorProfiles;
use stand_in::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authors_that_cannot_be_read_hold_up_nothing_else_of_the_chain() {
    let later = Arc::new(AtomicBool::new(false));
    let flag = later.clone();
    let chain = Chain::at(0x10);
    let indexer = an_indexer_on(
        chain.clone(),
        deployments(),
        Arc::new(move |request| {
            let later = flag.load(Ordering::SeqCst);
            match request["method"].as_str().unwrap_or_default() {
                "eth_call"
                    if later
                        && input(request)
                            .starts_with(&selector::<IAuthorProfiles::profileOfCall>()) =>
                {
                    Some(Err(format!(
                        "{REVERTED}: a read the profiles do not answer"
                    )))
                }
                _ => None,
            }
        }),
        "authors-unread",
    )
    .await;

    indexer.refresh().await.unwrap();
    {
        let snapshot = indexer.snapshot.read().await;
        assert_eq!(snapshot.chains[0].block, 0x10);
        assert_eq!(snapshot.authors.len(), 1, "the vault's author is read");
    }

    later.store(true, Ordering::SeqCst);
    chain.head.store(0x20, Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await;
    assert_eq!(
        snapshot.chains[0].block, 0x20,
        "the chain was not read again because its authors could not be"
    );
    assert_eq!(snapshot.strategies.len(), 1, "the vault is still shown");
    assert_eq!(
        snapshot.authors.len(),
        1,
        "the authors are kept as last read"
    );
}
