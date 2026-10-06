//! Two key clients made for one key, from one client, share the key's turn
//! to send as they share its nonce floor. Each send asks the node for the
//! key's count while it holds the turn; two sends at once from separate
//! turns would both be numbered from the same count, and the node would
//! keep only one of them.
//!
//! Needs the `args` feature.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::consensus::{Transaction, TxEnvelope};
use alloy::eips::eip2718::Decodable2718;
use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::signers::local::PrivateKeySigner;
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::json;
use stand_in::Said;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_key_clients_for_one_key_never_share_a_nonce() {
    let nonces: Arc<Mutex<Vec<u64>>> = Default::default();
    let answer: stand_in::Answer = {
        let nonces = nonces.clone();
        Arc::new(move |method, request| match method {
            "eth_getBlockByNumber" => {
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.number = 16;
                block.header.inner.timestamp = 1_700_000_000;
                block.header.inner.base_fee_per_gas = Some(1_000_000_000);
                Said::Result(serde_json::to_value(block).unwrap())
            }
            "eth_blockNumber" => Said::Result(json!("0x10")),
            "eth_gasPrice" | "eth_maxPriorityFeePerGas" => Said::Result(json!("0x3b9aca00")),
            "eth_feeHistory" => Said::Result(json!({
                "oldestBlock": "0x1",
                "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                "gasUsedRatio": [0.5],
                "reward": [["0x3b9aca00"]],
            })),
            "eth_estimateGas" => Said::Result(json!("0x5208")),
            // The pending count: everything the node has been sent.
            "eth_getTransactionCount" => {
                Said::Result(json!(format!("0x{:x}", nonces.lock().unwrap().len())))
            }
            "eth_sendRawTransaction" => {
                let raw = alloy::hex::decode(request["params"][0].as_str().unwrap()).unwrap();
                let tx = TxEnvelope::decode_2718(&mut raw.as_slice()).unwrap();
                nonces.lock().unwrap().push(tx.nonce());
                Said::Result(json!(keccak256(&raw)))
            }
            "eth_getTransactionReceipt" => Said::Result(json!({
                "transactionHash": request["params"][0],
                "transactionIndex": "0x0",
                "blockHash": B256::repeat_byte(0xbb),
                "blockNumber": "0x11",
                "from": Address::ZERO,
                "to": Address::repeat_byte(0x22),
                "cumulativeGasUsed": "0x5208",
                "gasUsed": "0x5208",
                "effectiveGasPrice": "0x3b9aca00",
                "contractAddress": null,
                "logs": [],
                "logsBloom": format!("0x{}", "00".repeat(256)),
                "status": "0x1",
                "type": "0x2",
            })),
            _ => stand_in::unknown_method(),
        })
    };
    let url = stand_in::answering(Some(31337), answer).await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    let key = PrivateKeySigner::random();
    let (one, two) = (
        client.with_key(key.clone()).unwrap(),
        client.with_key(key).unwrap(),
    );

    let call = |byte| IERC20::approveCall {
        spender: Address::repeat_byte(byte),
        value: U256::from(1u64),
    };
    let (first, second) = tokio::join!(
        one.send(Address::repeat_byte(0x22), call(0x33)),
        two.send(Address::repeat_byte(0x22), call(0x44)),
    );
    first.unwrap();
    second.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let nonces = nonces.lock().unwrap().clone();
    assert_eq!(nonces.len(), 2, "both were broadcast");
    assert_ne!(
        nonces[0], nonces[1],
        "two key clients for one key took the same nonce: the node keeps only one"
    );
}
