//! A key signs for the chain id the client checked against the chain config
//! and the deployment file when it connected, never one a node names at send
//! time. A node (or a gateway in front of it) that answers `eth_chainId`
//! truthfully while the client checks it and with another id afterwards
//! would otherwise be handed a transaction valid on that other chain, which
//! whoever runs the node could broadcast there: a vault key is
//! `keccak(image, vault)`, the same on every chain for a vault at the same
//! address, and a role key is the same on every chain too. As no service
//! signs a digest a node gave it, no key signs for a chain id one gave it.
//!
//! Both kinds of key are checked: one the service made itself
//! (`Client::with_key`, a vault's key), and the role keys a client connects
//! with (`Signing::Keys`). Needs the `args` feature.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::consensus::{Transaction, TxEnvelope};
use alloy::eips::eip2718::Decodable2718;
use alloy::primitives::{Address, U256, keccak256};
use alloy::signers::local::PrivateKeySigner;
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::json;
use stand_in::Said;

const CHECKED: u64 = 31_337;
const OTHER: u64 = 1;

/// A node that answers the client's check with [`CHECKED`] and every later
/// `eth_chainId` with [`OTHER`], noting the chain id of every transaction it
/// is sent. Where it is, and those chain ids.
async fn a_node_that_changes_its_chain_id() -> (String, Arc<Mutex<Vec<Option<u64>>>>) {
    let asked = Arc::new(AtomicUsize::new(0));
    let signed_for: Arc<Mutex<Vec<Option<u64>>>> = Default::default();
    let answer: stand_in::Answer = {
        let signed_for = signed_for.clone();
        Arc::new(move |method, request| match method {
            "eth_chainId" => {
                let id = if asked.fetch_add(1, Ordering::SeqCst) == 0 {
                    CHECKED
                } else {
                    OTHER
                };
                Said::Result(json!(format!("0x{id:x}")))
            }
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
            "eth_getTransactionCount" => Said::Result(json!("0x0")),
            "eth_sendRawTransaction" => {
                let raw = alloy::hex::decode(request["params"][0].as_str().unwrap()).unwrap();
                let tx = TxEnvelope::decode_2718(&mut raw.as_slice()).unwrap();
                signed_for.lock().unwrap().push(tx.chain_id());
                Said::Result(json!(keccak256(&raw)))
            }
            "eth_getTransactionReceipt" => Said::Result(serde_json::Value::Null),
            _ => stand_in::unknown_method(),
        })
    };
    // No fixed answer to eth_chainId: the closure gives it.
    (stand_in::answering(None, answer).await, signed_for)
}

fn approve() -> IERC20::approveCall {
    IERC20::approveCall {
        spender: Address::repeat_byte(0x33),
        value: U256::from(1u64),
    }
}

fn assert_signed_only_for_the_checked_chain(signed_for: &Mutex<Vec<Option<u64>>>, key: &str) {
    let signed_for = signed_for.lock().unwrap().clone();
    assert!(!signed_for.is_empty(), "nothing was broadcast");
    for id in signed_for {
        assert_eq!(
            id,
            Some(CHECKED),
            "{key} signed a transaction for chain {id:?}, which the node named after the \
             client had checked {CHECKED}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_key_never_signs_a_transaction_for_another_chain_id() {
    let (url, signed_for) = a_node_that_changes_its_chain_id().await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    assert_eq!(client.chain_id, CHECKED, "the connection checked the id");

    let key = client.with_key(PrivateKeySigner::random()).unwrap();
    // The receipt never comes; only what was signed matters.
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        key.send(Address::repeat_byte(0x22), approve()),
    )
    .await;
    assert_signed_only_for_the_checked_chain(&signed_for, "a vault key");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_role_key_never_signs_a_transaction_for_another_chain_id() {
    let (url, signed_for) = a_node_that_changes_its_chain_id().await;
    let role = PrivateKeySigner::random();
    let from = role.address();
    let client = chain::Client::connect(&stand_in::config(&url), Signing::Keys(vec![role]))
        .await
        .unwrap();
    assert_eq!(client.chain_id, CHECKED, "the connection checked the id");

    // The receipt never comes; only what was signed matters.
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        client.send(from, Address::repeat_byte(0x22), approve()),
    )
    .await;
    assert_signed_only_for_the_checked_chain(&signed_for, "a role key");
}
