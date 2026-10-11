//! Head details bind event hints to their number, hash and parent.
#![cfg(feature = "args")]
mod stand_in;

use alloy::primitives::{Address, B256, Bloom, BloomInput};
use chain::{Client, Signing};
use stand_in::Said;
use std::sync::Arc;

#[tokio::test]
async fn a_header_bloom_describes_only_the_observed_block() {
    let emitted = Address::repeat_byte(7);
    let absent = Address::repeat_byte(19);
    let url = stand_in::answering(
        Some(31337),
        Arc::new(move |method, _| {
            if method != "eth_getBlockByNumber" {
                return stand_in::unknown_method();
            }
            let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
            let mut bloom = Bloom::ZERO;
            bloom.accrue(BloomInput::Raw(emitted.as_slice()));
            block.header.inner.number = 12;
            block.header.inner.timestamp = 100;
            block.header.inner.parent_hash = B256::repeat_byte(11);
            block.header.inner.logs_bloom = bloom;
            block.header.hash = B256::repeat_byte(12);
            Said::Result(serde_json::to_value(block).unwrap())
        }),
    )
    .await;
    let client = Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    let head = client.head_details().await.unwrap();
    assert_eq!(
        (head.number, head.timestamp, head.hash, head.parent_hash),
        (12, 100, B256::repeat_byte(12), B256::repeat_byte(11))
    );
    assert!(head.may_have_logs(&[emitted]));
    assert!(head.may_have_logs(&[absent, emitted]));
    assert!(!head.may_have_logs(&[absent]));
}
