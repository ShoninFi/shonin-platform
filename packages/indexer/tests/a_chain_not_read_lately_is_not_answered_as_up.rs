//! The health route answers 503 while a vault chain it is to read is not
//! connected, or has not been read lately, and its first line, short enough
//! for a dashboard's row, says which: a chain that never connects is not
//! shown as up.

mod stand_in;

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use chain::Served;
use indexer::{HEALTH_LINE, Indexer, STALE_AFTER};

/// Base, on a node at block 0x10 with no vaults and no logs.
async fn served() -> Served {
    stand_in::served_on(
        stand_in::Chain::at(stand_in::HEAD),
        stand_in::deployments_from(0),
        stand_in::answering_no_vaults(),
    )
    .await
}

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-indexer-health-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn first_line(said: &str) -> &str {
    said.lines().next().unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_that_never_connects_is_not_answered_as_up() {
    let indexer = Indexer::new(
        vec![shared::Chain::Base, shared::Chain::Hyperliquid],
        scratch(),
        None,
    )
    .with_rpc_policy(stand_in::policy());
    indexer.add(served().await).await;
    tokio::time::timeout(Duration::from_secs(45), indexer.refresh())
        .await
        .expect("a refresh finishes")
        .unwrap();

    let (status, said) = indexer.health_answer("indexer");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{said}");
    assert_eq!(first_line(&said), "indexer: hyperliquid not connected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chain_read_lately_is_up_and_one_not_read_lately_is_not() {
    let indexer = Indexer::new(vec![shared::Chain::Base], scratch(), None)
        .with_rpc_policy(stand_in::policy());
    indexer.add(served().await).await;
    tokio::time::timeout(Duration::from_secs(45), indexer.refresh())
        .await
        .expect("a refresh finishes")
        .unwrap();

    let (status, said) = indexer.health_answer("indexer");
    assert_eq!(status, StatusCode::OK, "{said}");
    assert!(said.contains("base: read"), "{said}");

    // As it would read once the chain has gone unread past the limit.
    let later = Instant::now() + STALE_AFTER + Duration::from_secs(60);
    let (up, said) = indexer.health_at("indexer", later);
    assert!(!up, "{said}");
    assert!(
        first_line(&said).starts_with("indexer: base not read for"),
        "{said}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_line_stays_short_however_many_chains_are_wrong() {
    let indexer = Indexer::new(
        vec![
            shared::Chain::Base,
            shared::Chain::Hyperliquid,
            shared::Chain::Robinhood,
            shared::Chain::Solana,
        ],
        scratch(),
        None,
    );
    let (up, said) = indexer.health_at("indexer", Instant::now());
    assert!(!up);
    assert_eq!(first_line(&said), "indexer: 4 chains not read");
    assert!(first_line(&said).chars().count() <= HEALTH_LINE);
    // The details follow, one chain a line.
    assert_eq!(said.lines().count(), 5, "{said}");
}
