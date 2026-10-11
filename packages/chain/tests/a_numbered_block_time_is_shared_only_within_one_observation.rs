//! Numbered header times share a bounded observation; live reads stay fresh.
#![cfg(feature = "args")]
mod stand_in;

use alloy::primitives::B256;
use chain::{Client, Signing};
use serde_json::json;
use stand_in::Said;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn header_times_are_shared_and_discarded_at_the_next_observation() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let requests = seen.clone();
    let url = stand_in::answering(
        Some(31337),
        Arc::new(move |method, request| match method {
            "eth_blockNumber" => Said::Result(json!("0xc")),
            "eth_getBlockByNumber" => {
                let number = u64::from_str_radix(
                    request["params"][0]
                        .as_str()
                        .unwrap()
                        .trim_start_matches("0x"),
                    16,
                )
                .unwrap();
                requests.lock().unwrap().push(number);
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.number = number;
                block.header.inner.timestamp = 100 + number;
                block.header.hash = B256::repeat_byte(number as u8);
                Said::Result(serde_json::to_value(block).unwrap())
            }
            _ => stand_in::unknown_method(),
        }),
    )
    .await;
    let client = Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    seen.lock().unwrap().clear();
    let observed = client.verifying_logs(12, B256::repeat_byte(12));
    for _ in 0..3 {
        assert_eq!(observed.block_timestamp_at(10).await.unwrap(), 110);
    }
    assert_eq!(*seen.lock().unwrap(), [10]);
    // Another historical block is shared independently.
    for _ in 0..2 {
        assert_eq!(observed.block_timestamp_at(11).await.unwrap(), 111);
    }
    assert_eq!(*seen.lock().unwrap(), [10, 11]);
    assert!(observed.block_timestamp_at(13).await.is_err());
    // The publication witness always bypasses the observation.
    assert_eq!(
        observed.block_identity(10).await.unwrap(),
        B256::repeat_byte(10)
    );
    let next = client.verifying_logs(12, B256::repeat_byte(12));
    assert_eq!(next.block_timestamp_at(10).await.unwrap(), 110);
    for _ in 0..2 {
        assert_eq!(client.block_timestamp_at(10).await.unwrap(), 110);
    }
    assert_eq!(*seen.lock().unwrap(), [10, 11, 10, 10, 10, 10]);
    let replaced = client.verifying_logs(12, B256::repeat_byte(99));
    assert!(replaced.block_timestamp_at(12).await.is_err());
}
