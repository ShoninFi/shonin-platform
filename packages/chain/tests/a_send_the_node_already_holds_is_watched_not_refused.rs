//! "already known" is not a refusal.
//!
//! A node answers `already known` when its pool already holds this exact
//! transaction. A key signs deterministically, so a resend filled in as an
//! earlier one was (same nonce, which a gateway whose backends lag still
//! reports as free, same fees, same call) is that transaction. The node has
//! it and it may land, so the send watches for it as for any other and says
//! what became of it, never that it was refused.
//!
//! Needs the `args` feature.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use alloy::primitives::{Address, B256, U256};
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
async fn a_send_the_node_already_holds_is_not_reported_as_refused() {
    let broadcasts = Arc::new(AtomicUsize::new(0));
    let mined = Arc::new(AtomicBool::new(false));
    let answer: stand_in::Answer = {
        let broadcasts = broadcasts.clone();
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
            // A backend that has not seen the first yet.
            "eth_getTransactionCount" => Said::Result(json!("0x0")),
            "eth_sendRawTransaction" => {
                if broadcasts.fetch_add(1, Ordering::SeqCst) == 0 {
                    // Taken, and the reply lost on the way back.
                    Said::Raw(502, "bad gateway".to_string())
                } else {
                    Said::Raw(
                        200,
                        r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32000,"message":"already known"}}"#
                            .to_string(),
                    )
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

    let first = {
        let key = key.clone();
        tokio::spawn(async move { key.send(Address::repeat_byte(0x22), approve()).await })
    };
    while broadcasts.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::pause();
    let _ = first
        .await
        .unwrap()
        .expect_err("the first send got no answer");
    tokio::time::resume();

    // The same order again. The node already holds it, and mines it.
    let second = {
        let key = key.clone();
        tokio::spawn(async move { key.send(Address::repeat_byte(0x22), approve()).await })
    };
    while broadcasts.load(Ordering::SeqCst) < 2 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    mined.store(true, Ordering::SeqCst);
    second
        .await
        .unwrap()
        .expect("the node holds this very transaction and it landed");
}
