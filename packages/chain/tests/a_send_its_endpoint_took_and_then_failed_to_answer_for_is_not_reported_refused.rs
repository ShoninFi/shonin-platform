//! A send its endpoint took, and then answered with a failure of its own, is
//! not a refused send.
//!
//! A gateway hands a transaction to its node and loses the node's reply.
//! Some say so with an HTTP status (a 502, a 504); others answer 200 with a
//! JSON-RPC error of their own: an internal error, an upstream timeout. That
//! error says nothing about the transaction, which the node has and mines.
//! Reported as a refusal, the sender is told nothing was sent, may send the
//! same action again, and the gas of the one that landed is never counted.
//!
//! Only an answer about the transaction itself (its nonce, its funds, its
//! fee, its bytes) is a refusal. An endpoint's own failure is no answer,
//! however it is delivered: whether the transaction landed is found out by
//! its hash.
//!
//! Needs the `args` feature.

#![cfg(feature = "args")]

mod stand_in;

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

/// One endpoint: a gateway whose node takes the transaction and mines it at
/// once, and which then answers the send with `failure`, a JSON-RPC error of
/// its own, in an HTTP 200.
fn gateway(mined: Arc<Mutex<Option<B256>>>, failure: Value) -> stand_in::Answer {
    Arc::new(move |method, request| match method {
        "eth_getBlockByNumber" => {
            let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
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
        "eth_getTransactionCount" => {
            let count = u64::from(mined.lock().unwrap().is_some());
            Said::Result(json!(format!("0x{count:x}")))
        }
        "eth_sendRawTransaction" => {
            let raw = alloy::hex::decode(request["params"][0].as_str().unwrap()).unwrap();
            *mined.lock().unwrap() = Some(keccak256(&raw));
            Said::Raw(
                200,
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "error": failure.clone(),
                })
                .to_string(),
            )
        }
        "eth_getTransactionReceipt" => {
            let asked: B256 = request["params"][0].as_str().unwrap().parse().unwrap();
            if *mined.lock().unwrap() == Some(asked) {
                Said::Result(receipt(&request["params"][0]))
            } else {
                Said::Result(Value::Null)
            }
        }
        _ => stand_in::unknown_method(),
    })
}

/// What gateways say of their own trouble inside a JSON-RPC error.
fn failures() -> Vec<Value> {
    vec![
        json!({"code": -32603, "message": "Internal error"}),
        json!({"code": -32000, "message": "upstream request timeout"}),
        json!({"code": 30, "message": "Request timeout on the free tier, please upgrade your tier to the paid one"}),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_s_send_that_landed_is_not_called_refused_for_its_gateway_s_own_failure() {
    for failure in failures() {
        let mined: Arc<Mutex<Option<B256>>> = Default::default();
        let only = stand_in::answering(Some(31337), gateway(mined.clone(), failure.clone())).await;
        let client = chain::Client::connect(&stand_in::config(&only), Signing::ReadOnly)
            .await
            .unwrap();
        let key = client.with_key(PrivateKeySigner::random()).unwrap();
        let outcome = key.send(Address::repeat_byte(0x22), approve()).await;
        assert!(
            mined.lock().unwrap().is_some(),
            "the stand-in never took the send, so the test proves nothing"
        );
        if let Err(error) = &outcome {
            assert!(
                chain::may_still_land(error),
                "the transaction is mined, and its sender was told it was refused \
                 (the gateway said {failure}): {error:#}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_gas_of_a_send_that_landed_behind_its_gateway_s_own_failure_is_counted() {
    let mined: Arc<Mutex<Option<B256>>> = Default::default();
    let only = stand_in::answering(Some(31337), gateway(mined.clone(), failures().remove(0))).await;
    let client = chain::Client::connect(&stand_in::config(&only), Signing::ReadOnly)
        .await
        .unwrap();
    let key = client.with_key(PrivateKeySigner::random()).unwrap();
    let _ = key.send(Address::repeat_byte(0x22), approve()).await;
    assert!(mined.lock().unwrap().is_some());
    for _ in 0..40 {
        if key.gas_spent() > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("the transaction is mined and paid for, and its gas was never counted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_role_account_s_send_that_landed_is_not_called_refused_for_its_gateway_s_own_failure() {
    let mined: Arc<Mutex<Option<B256>>> = Default::default();
    let only = stand_in::answering(Some(31337), gateway(mined.clone(), failures().remove(0))).await;
    let signer = PrivateKeySigner::random();
    let from = signer.address();
    let client = chain::Client::connect(&stand_in::config(&only), Signing::Keys(vec![signer]))
        .await
        .unwrap();
    let outcome = client
        .send(from, Address::repeat_byte(0x22), approve())
        .await;
    assert!(mined.lock().unwrap().is_some());
    if let Err(error) = &outcome {
        assert!(
            chain::may_still_land(error),
            "the transaction is mined, and its sender was told it was refused: {error:#}"
        );
    }
}
