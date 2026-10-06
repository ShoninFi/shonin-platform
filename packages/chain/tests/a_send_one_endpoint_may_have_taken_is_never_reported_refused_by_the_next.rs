//! A send that one endpoint took without saying so is not a refused send
//! because the next endpoint calls its nonce too low.
//!
//! A chain may name several endpoints, and a request one of them fails goes
//! on to the next: a send too, as the same signed bytes. A gateway that
//! hands the transaction to its node and then fails the reply (a 502, a 504,
//! a dropped connection) has still taken it, and on a chain with fast blocks
//! it is mined before the next endpoint is asked. That endpoint then answers
//! `nonce too low`: the account's nonce has moved, because this very
//! transaction used it. Reported as a refusal, the caller is told nothing
//! was sent and may send the same action again, and the gas of the one that
//! landed is never counted.
//!
//! So once an endpoint may have taken the bytes, no later endpoint's
//! refusal is taken as one: whether it landed is found out by its hash. A
//! refusal from the first endpoint that answers stays a refusal.
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

/// One chain behind two endpoints. The first endpoint handed a transaction
/// takes it, and its gateway fails the reply; the transaction is mined at
/// once. Any endpoint handed it afterwards says its nonce is too low.
fn endpoint(mined: Arc<Mutex<Option<B256>>>) -> stand_in::Answer {
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
            let mut mined = mined.lock().unwrap();
            if mined.is_none() {
                *mined = Some(keccak256(&raw));
                Said::Raw(504, "upstream request timeout".to_string())
            } else {
                Said::Raw(
                    200,
                    json!({
                        "jsonrpc": "2.0",
                        "id": request["id"].clone(),
                        "error": {"code": -32000, "message": "nonce too low"},
                    })
                    .to_string(),
                )
            }
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

async fn key_over_two_endpoints() -> (chain::KeyClient, Arc<Mutex<Option<B256>>>) {
    let mined: Arc<Mutex<Option<B256>>> = Default::default();
    let first = stand_in::answering(Some(31337), endpoint(mined.clone())).await;
    let second = stand_in::answering(Some(31337), endpoint(mined.clone())).await;
    let client = chain::Client::connect(
        &stand_in::config(&format!("{first} {second}")),
        Signing::ReadOnly,
    )
    .await
    .unwrap();
    let key = client.with_key(PrivateKeySigner::random()).unwrap();
    (key, mined)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_that_landed_through_the_first_endpoint_is_not_called_refused() {
    let (key, mined) = key_over_two_endpoints().await;
    let outcome = key.send(Address::repeat_byte(0x22), approve()).await;
    assert!(
        mined.lock().unwrap().is_some(),
        "the stand-in never took the send, so the test proves nothing"
    );
    if let Err(error) = &outcome {
        assert!(
            chain::may_still_land(error),
            "the transaction is mined, and its sender was told it was refused: {error:#}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_gas_of_a_send_that_landed_through_the_first_endpoint_is_counted() {
    let (key, mined) = key_over_two_endpoints().await;
    let _ = key.send(Address::repeat_byte(0x22), approve()).await;
    assert!(mined.lock().unwrap().is_some());
    // The receipt is there from the moment the send was taken; a watch for
    // it finds it within a poll or two.
    for _ in 0..40 {
        if key.gas_spent() > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("the transaction is mined and paid for, and its gas was never counted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_role_account_s_send_that_landed_through_the_first_endpoint_is_not_called_refused() {
    let mined: Arc<Mutex<Option<B256>>> = Default::default();
    let first = stand_in::answering(Some(31337), endpoint(mined.clone())).await;
    let second = stand_in::answering(Some(31337), endpoint(mined.clone())).await;
    let signer = PrivateKeySigner::random();
    let from = signer.address();
    let client = chain::Client::connect(
        &stand_in::config(&format!("{first} {second}")),
        Signing::Keys(vec![signer]),
    )
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_from_the_first_endpoint_asked_stays_a_refusal() {
    let asked_second = Arc::new(Mutex::new(false));
    // The first endpoint refuses outright; it took nothing.
    let refusing: stand_in::Answer = {
        let inner = endpoint(Default::default());
        Arc::new(move |method, request| match method {
            "eth_sendRawTransaction" => Said::Raw(
                200,
                json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "error": {"code": -32000, "message": "nonce too low"},
                })
                .to_string(),
            ),
            _ => inner(method, request),
        })
    };
    let second: stand_in::Answer = {
        let inner = endpoint(Default::default());
        let asked_second = asked_second.clone();
        Arc::new(move |method, request| {
            if method == "eth_sendRawTransaction" {
                *asked_second.lock().unwrap() = true;
            }
            inner(method, request)
        })
    };
    let first = stand_in::answering(Some(31337), refusing).await;
    let second = stand_in::answering(Some(31337), second).await;
    let client = chain::Client::connect(
        &stand_in::config(&format!("{first} {second}")),
        Signing::ReadOnly,
    )
    .await
    .unwrap();
    let key = client.with_key(PrivateKeySigner::random()).unwrap();
    let error = key
        .send(Address::repeat_byte(0x22), approve())
        .await
        .expect_err("the first endpoint refused it");
    assert!(
        !chain::may_still_land(&error),
        "a refusal was taken for a send that may land: {error:#}"
    );
    assert!(format!("{error:#}").contains("nonce too low"), "{error:#}");
    assert!(
        !*asked_second.lock().unwrap(),
        "a refused send was handed to a second endpoint"
    );
}
