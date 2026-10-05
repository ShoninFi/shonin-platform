//! A chain whose node never answers: the keeper keeps nothing at all, and
//! its health says so from the start rather than answering as up.

mod process;

use std::time::Duration;

use process::start_keeper;

#[tokio::test(flavor = "multi_thread")]
async fn a_chain_that_never_connects_is_not_answered_as_up() {
    let keeper = start_keeper("never-connects", "http://127.0.0.1:1", "http://127.0.0.1:1");
    let health = keeper.health_until(Duration::from_secs(15), |_| true).await;
    // Give it a few seconds of trying.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let health = keeper.health().await.or(health);
    let (status, said) = health.clone().unwrap_or_default();
    assert_eq!(
        status, 503,
        "its only chain has never connected, yet /health answered {health:?}"
    );
    assert!(said.contains("base (not connected)"), "{said:?}");
}
