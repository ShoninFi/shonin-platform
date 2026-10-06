//! A key client (an executor's vault key, the keeper's) numbers each send at
//! the node's count of the key's sends, but never below the nonce after the
//! key's last send it saw land: a public endpoint is often several nodes
//! behind one name, and one a block behind would number the next send the
//! same as the last. A client started again has seen nothing land, so it
//! takes the node's count.
//!
//! Needs the `args` feature.

#![cfg(feature = "args")]

mod stand_in;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alloy::consensus::{Transaction, TxEnvelope};
use alloy::eips::eip2718::Decodable2718;
use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::signers::local::PrivateKeySigner;
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::json;
use stand_in::Said;

/// What the node has been sent, and the count it gives each sender: a
/// lagging node's count, set by the test.
#[derive(Default)]
struct Node {
    sent: Vec<(Address, u64)>,
    counts: HashMap<Address, u64>,
}

fn node(state: Arc<Mutex<Node>>) -> stand_in::Answer {
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
            let who: Address = request["params"][0].as_str().unwrap().parse().unwrap();
            let count = state.lock().unwrap().counts.get(&who).copied().unwrap_or(0);
            Said::Result(json!(format!("0x{count:x}")))
        }
        "eth_sendRawTransaction" => {
            let raw = alloy::hex::decode(request["params"][0].as_str().unwrap()).unwrap();
            let tx = TxEnvelope::decode_2718(&mut raw.as_slice()).unwrap();
            let from =
                alloy::consensus::transaction::SignerRecoverable::recover_signer(&tx).unwrap();
            state.lock().unwrap().sent.push((from, tx.nonce()));
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
}

fn approve(byte: u8) -> IERC20::approveCall {
    IERC20::approveCall {
        spender: Address::repeat_byte(byte),
        value: U256::from(1u64),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lagging_node_s_count_after_a_landed_send_is_not_taken_for_the_next_nonce() {
    let state: Arc<Mutex<Node>> = Default::default();
    let url = stand_in::answering(Some(31337), node(state.clone())).await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    let (one, two) = (PrivateKeySigner::random(), PrivateKeySigner::random());
    let (a, b) = (one.address(), two.address());
    let first = client.with_key(one).unwrap();
    let second = client.with_key(two).unwrap();

    state.lock().unwrap().counts.insert(a, 3);
    first
        .send(Address::repeat_byte(0x22), approve(1))
        .await
        .unwrap();
    // It landed as 3, and the node answering next is a block behind: it
    // still counts three.
    first
        .send(Address::repeat_byte(0x22), approve(2))
        .await
        .unwrap();
    // The other key's count is its own.
    second
        .send(Address::repeat_byte(0x22), approve(3))
        .await
        .unwrap();
    let sent = state.lock().unwrap().sent.clone();
    assert_eq!(sent, vec![(a, 3), (a, 4), (b, 0)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_key_client_made_from_one_client_shares_the_floor_and_a_restart_starts_from_the_node()
{
    let state: Arc<Mutex<Node>> = Default::default();
    let url = stand_in::answering(Some(31337), node(state.clone())).await;
    let key = PrivateKeySigner::from_bytes(&B256::repeat_byte(0x42)).unwrap();
    let address = key.address();

    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    state.lock().unwrap().counts.insert(address, 8);
    client
        .with_key(key.clone())
        .unwrap()
        .send(Address::repeat_byte(0x22), approve(1))
        .await
        .unwrap();
    // A second key client for the same key, made from the same client,
    // knows the first's send landed.
    client
        .with_key(key.clone())
        .unwrap()
        .send(Address::repeat_byte(0x22), approve(2))
        .await
        .unwrap();

    // The service starts again: a new client has seen nothing land, and the
    // node's count is all it has. The node has caught up meanwhile.
    state.lock().unwrap().counts.insert(address, 10);
    let restarted = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    restarted
        .with_key(key)
        .unwrap()
        .send(Address::repeat_byte(0x22), approve(3))
        .await
        .unwrap();

    let sent: Vec<u64> = state
        .lock()
        .unwrap()
        .sent
        .iter()
        .map(|(_, nonce)| *nonce)
        .collect();
    assert_eq!(sent, vec![8, 9, 10]);
}
