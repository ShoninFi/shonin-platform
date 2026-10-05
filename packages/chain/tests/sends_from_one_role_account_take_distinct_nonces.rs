//! Two sends from one role account, in one service, at once.
//!
//! On a testnet a client signs the role accounts' transactions itself
//! (`Signing::Keys`), asking the node for the pending nonce each time. A
//! service that renews its blessing while it registers a build, or tops up
//! one vault's key while it stakes another, sends from the same account at
//! the same moment. Each send takes the account's turn, shared by every
//! clone of the client, so the second asks for its nonce only once the
//! first has been taken, and the node keeps both.
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
use serde_json::{Value, json};
use stand_in::Said;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_sends_from_one_role_account_never_share_a_nonce() {
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
            _ => {
                let _: &Value = request;
                stand_in::unknown_method()
            }
        })
    };
    let url = stand_in::answering(Some(31337), answer).await;
    let signer = PrivateKeySigner::random();
    let from = signer.address();
    let client = chain::Client::connect(&stand_in::config(&url), Signing::Keys(vec![signer]))
        .await
        .unwrap();

    let call = |byte| IERC20::approveCall {
        spender: Address::repeat_byte(byte),
        value: U256::from(1u64),
    };
    // A blessing's renewal and a registration, at the same moment.
    let (one, two) = tokio::join!(
        client.send(from, Address::repeat_byte(0x22), call(0x33)),
        client.send(from, Address::repeat_byte(0x22), call(0x44)),
    );
    one.unwrap();
    two.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let nonces = nonces.lock().unwrap().clone();
    assert_eq!(nonces.len(), 2, "both were broadcast");
    assert_ne!(
        nonces[0], nonces[1],
        "two sends from one account took the same nonce: the node keeps only one of them"
    );
}
