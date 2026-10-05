//! Connecting to a chain gives up once it has had two minutes of no answer,
//! by the clock: a node that takes each request and never replies spends a
//! request's whole timeout on every try, and counting only the pauses
//! between tries would wait hours. Every service connects to its chains
//! before it does anything else, so one silent gateway would hold it all.
//! Needs the `args` feature, as every service has it.
//!
//! The clock is paused, so the test takes no real time.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::Arc;
use std::time::Duration;

use chain::Signing;

#[tokio::test]
async fn connecting_to_a_node_that_never_answers_gives_up_in_about_two_minutes() {
    let answer: stand_in::Answer = Arc::new(|_method, _request| stand_in::Said::Silence);
    let url = stand_in::answering(None, answer).await;
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(4 * 3_600),
        chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly),
    )
    .await;
    let waited = started.elapsed();
    tokio::time::resume();
    match outcome {
        Ok(Err(_)) => assert!(
            waited < Duration::from_secs(5 * 60),
            "it gave up, but only after {waited:?}"
        ),
        Ok(Ok(_)) => panic!("nothing answered, yet it connected"),
        Err(_) => panic!("still trying to connect four hours on"),
    }
}
