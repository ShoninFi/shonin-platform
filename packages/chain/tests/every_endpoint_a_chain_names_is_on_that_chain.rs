//! Every endpoint of a chain's `rpc` list is checked at connect to be on
//! that chain, as the log endpoints are. Stations share one address on
//! every chain, so an endpoint for another chain would answer for that
//! chain as this one, and the failover may pick it for any request. The
//! error names the endpoint by its place in the list, never by its URL,
//! which may carry a key.
//!
//! Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::Arc;

use chain::Signing;
use serde_json::json;
use stand_in::Said;

const SECRET: &str = "rpc-key-0123456789abcdef";

fn node() -> stand_in::Answer {
    Arc::new(|method, _request| match method {
        "eth_blockNumber" => Said::Result(json!("0x10")),
        _ => stand_in::unknown_method(),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_rpc_endpoint_on_another_chain_is_refused_without_naming_its_url() {
    let ours = stand_in::answering(Some(31337), node()).await;
    let other = stand_in::answering(Some(8453), node()).await;
    let mut config = stand_in::config(&format!("{ours} {other}/v2/{SECRET}"));
    config.label = "stand-in".to_string();
    let Err(error) = chain::Client::connect(&config, Signing::ReadOnly).await else {
        panic!("an endpoint on chain 8453 was kept in a list for chain 31337");
    };
    let text = format!("{error:#} / {error:?}");
    assert!(text.contains("8453") && text.contains("rpc"), "{text}");
    assert!(!text.contains(SECRET), "the error carries the key: {text}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_list_all_on_one_chain_connects() {
    let one = stand_in::answering(Some(31337), node()).await;
    let two = stand_in::answering(Some(31337), node()).await;
    chain::Client::connect(
        &stand_in::config(&format!("{one} {two}")),
        Signing::ReadOnly,
    )
    .await
    .unwrap();
}
