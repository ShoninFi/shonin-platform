//! Capability, code and aggregation remain on one dedicated endpoint.
#![cfg(feature = "args")]
mod stand_in;

use alloy::primitives::{Address, B256, U256};
use chain::{
    Client, Signing,
    sealed_views::{Proofs, SealedViews},
};
use serde_json::{Value, json};
use stand_in::Said;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy)]
enum Kind {
    Honors,
    HonorsResource,
    Ignores,
    Unsupported,
    UnknownSucceeds,
    UnknownTransport,
    UnknownLimited,
}

fn helper(number: u64) -> Value {
    let words = [
        B256::from(U256::from(number)),
        B256::from(U256::from(100 + number)),
        B256::repeat_byte((number - 1) as u8),
        B256::from(U256::from(128)),
        B256::ZERO,
    ];
    json!(format!(
        "0x{}",
        alloy::hex::encode(
            words
                .into_iter()
                .flat_map(|word| word.to_vec())
                .collect::<Vec<_>>()
        )
    ))
}

async fn endpoint(kind: Kind, seen: Arc<Mutex<Vec<String>>>) -> String {
    stand_in::answering(Some(31337), Arc::new(move |method, request| {
        seen.lock().unwrap().push(method.into());
        match method {
            "eth_blockNumber" => Said::Result(json!("0xc")),
            "eth_getBlockByNumber" => {
                let number = request["params"][0].as_str().and_then(|tag| tag.strip_prefix("0x"))
                    .and_then(|hex| u64::from_str_radix(hex, 16).ok()).unwrap_or(12);
                let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.number = number;
                block.header.inner.timestamp = 100 + number;
                block.header.inner.parent_hash = B256::repeat_byte((number - 1) as u8);
                block.header.hash = B256::repeat_byte(number as u8);
                Said::Result(serde_json::to_value(block).unwrap())
            }
            "eth_call" => {
                assert_eq!(request["params"][1]["requireCanonical"], true);
                let parent = request["params"][1]["blockHash"] == json!(B256::repeat_byte(11));
                match kind {
                    Kind::Ignores => Said::Result(helper(12)),
                    Kind::Unsupported => Said::Raw(200, json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32602,"message":"unsupported argument"}}).to_string()),
                    _ if parent => Said::Result(helper(11)),
                    Kind::UnknownSucceeds => Said::Result(helper(12)),
                    Kind::UnknownTransport => Said::Raw(503, "unavailable".into()),
                    Kind::UnknownLimited => Said::Raw(200, json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"rate limited"}}).to_string()),
                    Kind::HonorsResource => Said::Raw(200, json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32001,"message":"Resource not found"}}).to_string()),
                    Kind::Honors => Said::Raw(200, json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"header not found"}}).to_string()),
                }
            }
            "eth_getCode" => Said::Result(json!("0x")),
            _ => stand_in::unknown_method(),
        }
    })).await
}

#[tokio::test]
async fn missing_hash_is_proved_only_by_a_specific_node_refusal_after_a_historical_pin() {
    for kind in [
        Kind::Honors,
        Kind::HonorsResource,
        Kind::Ignores,
        Kind::Unsupported,
        Kind::UnknownSucceeds,
        Kind::UnknownTransport,
        Kind::UnknownLimited,
    ] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let url = endpoint(kind, seen.clone()).await;
        let client = Client::connect(&stand_in::config(&url), Signing::ReadOnly)
            .await
            .unwrap();
        seen.lock().unwrap().clear();
        let mut proofs = Proofs::default();
        let error = SealedViews::observe(
            &client,
            Address::repeat_byte(1),
            (12, 112, B256::repeat_byte(12)),
            &mut proofs,
        )
        .await
        .err()
        .unwrap();
        let code_reads = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|method| *method == "eth_getCode")
            .count();
        if matches!(kind, Kind::Honors | Kind::HonorsResource) {
            assert_eq!(code_reads, 1, "a proved endpoint reaches code verification");
            assert!(error.to_string().contains("fixed implementation clone"));
        } else {
            assert_eq!(
                code_reads, 0,
                "an unproved hash pin must not authorize any code proof"
            );
        }
    }
}

#[tokio::test]
async fn another_configured_endpoint_cannot_supply_the_capability_or_take_over_the_observation() {
    let first = Arc::new(Mutex::new(Vec::new()));
    let second = Arc::new(Mutex::new(Vec::new()));
    let first_url = endpoint(Kind::Ignores, first.clone()).await;
    let second_url = endpoint(Kind::Honors, second.clone()).await;
    let client = Client::connect(
        &stand_in::config(&format!("{first_url} {second_url}")),
        Signing::ReadOnly,
    )
    .await
    .unwrap();
    first.lock().unwrap().clear();
    second.lock().unwrap().clear();
    let mut proofs = Proofs::default();
    assert!(
        SealedViews::observe(
            &client,
            Address::repeat_byte(1),
            (12, 112, B256::repeat_byte(12)),
            &mut proofs
        )
        .await
        .is_err()
    );
    assert!(
        first
            .lock()
            .unwrap()
            .iter()
            .any(|method| method == "eth_call")
    );
    assert!(
        second.lock().unwrap().is_empty(),
        "aggregate probes and reads must never cross endpoints"
    );
}
