//! One transaction's gas is counted once, however many sends watched for it.
//!
//! A key signs deterministically, so a send that is filled in exactly as an
//! earlier one was (same nonce, same fees, same gas, same call) is the same
//! transaction, with the same hash. When the first send's broadcast got no
//! answer, its landing is still watched in the background when the caller
//! sends the same call again. If the node never had the first, the second is
//! the same bytes, it lands once, and both watches find its one receipt: the
//! key paid for it once, so its count goes up once.
//!
//! Needs the `args` feature.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::signers::local::PrivateKeySigner;
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::{Value, json};
use stand_in::Said;

fn approve() -> IERC20::approveCall {
    IERC20::approveCall {
        spender: Address::repeat_byte(0x33),
        value: U256::from(1u64),
    }
}

fn receipt(hash: &Value) -> Value {
    json!({
        "transactionHash": hash,
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
    })
}

#[tokio::test]
async fn gas_of_a_transaction_sent_twice_as_the_same_bytes_is_counted_once() {
    // The node: the first broadcast is lost at a gateway (a 502 that never
    // reached the node), so its nonce is still free; the second is taken and
    // mined.
    let broadcasts = Arc::new(AtomicUsize::new(0));
    let hashes: Arc<Mutex<Vec<B256>>> = Default::default();
    let mined = Arc::new(AtomicBool::new(false));
    let answer: stand_in::Answer = {
        let broadcasts = broadcasts.clone();
        let hashes = hashes.clone();
        let mined = mined.clone();
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
            // The node never had the first, so the nonce is still the same.
            "eth_getTransactionCount" => Said::Result(json!("0x0")),
            "eth_sendRawTransaction" => {
                let raw = request["params"][0].as_str().unwrap();
                let hash = keccak256(alloy::hex::decode(raw).unwrap());
                hashes.lock().unwrap().push(hash);
                if broadcasts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Said::Raw(502, "bad gateway".to_string())
                } else {
                    Said::Result(json!(hash))
                }
            }
            "eth_getTransactionReceipt" => {
                if mined.load(Ordering::SeqCst) {
                    Said::Result(receipt(&request["params"][0]))
                } else {
                    Said::Result(Value::Null)
                }
            }
            _ => stand_in::unknown_method(),
        })
    };
    let url = stand_in::answering(Some(31337), answer).await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    let key = client.with_key(PrivateKeySigner::random()).unwrap();

    // The first send: no answer, so it is given up on, and its landing is
    // still watched.
    let first = {
        let key = key.clone();
        tokio::spawn(async move { key.send(Address::repeat_byte(0x22), approve()).await })
    };
    while broadcasts.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::pause();
    let error = first
        .await
        .unwrap()
        .expect_err("the first send got no answer");
    tokio::time::resume();
    assert!(chain::unanswered(&error), "{error:#}");

    // The agent sends the same order again; this time it is taken and mined.
    let second = {
        let key = key.clone();
        tokio::spawn(async move { key.send(Address::repeat_byte(0x22), approve()).await })
    };
    while broadcasts.load(Ordering::SeqCst) < 2 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    {
        let hashes = hashes.lock().unwrap();
        assert_eq!(
            hashes[0], hashes[1],
            "the scenario needs the second send to be the same transaction"
        );
    }
    mined.store(true, Ordering::SeqCst);
    second.await.unwrap().expect("the second send landed");

    // Give the first send's watch time to look again.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        key.gas_spent(),
        21_000 * 1_000_000_000,
        "one transaction landed, and its gas was counted {} times",
        key.gas_spent() / (21_000 * 1_000_000_000)
    );
}
