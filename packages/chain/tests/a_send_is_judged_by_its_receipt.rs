//! A send's outcome is what its receipt says, once there is one. A read of
//! the receipt that fails on the way (a gateway's rate limit, a node
//! restarting) says nothing about the transaction, so it is asked again
//! until the wait is over rather than taken for a failed send, which its
//! sender would send again. And a receipt that says the chain reverted a
//! deployment is an error, though it names the address the contract would
//! have had. Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use alloy::primitives::{Address, B256, Bytes, U256};
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::{Value, json};
use stand_in::Said;

const HASH: B256 = B256::repeat_byte(0xaa);

/// A receipt for the one transaction the node is sent.
fn receipt(status: bool) -> Value {
    json!({
        "transactionHash": HASH,
        "transactionIndex": "0x0",
        "blockHash": B256::repeat_byte(0xbb),
        "blockNumber": "0x11",
        "from": Address::repeat_byte(0x01),
        "to": null,
        "cumulativeGasUsed": "0x5208",
        "gasUsed": "0x5208",
        "effectiveGasPrice": "0x3b9aca00",
        "contractAddress": Address::repeat_byte(0x77),
        "logs": [],
        "logsBloom": format!("0x{}", "00".repeat(256)),
        "status": if status { "0x1" } else { "0x0" },
        "type": "0x2",
    })
}

/// A node that signs for its accounts, takes every transaction, and has
/// its receipt for whoever asks, after `refusals` failed reads.
async fn client(refusals: usize, status: bool) -> chain::Client {
    let reads = Arc::new(AtomicUsize::new(0));
    let answer: stand_in::Answer = Arc::new(move |method, _request| match method {
        "eth_sendTransaction" => Said::Result(json!(HASH)),
        "eth_getTransactionReceipt" => {
            if reads.fetch_add(1, Ordering::SeqCst) < refusals {
                Said::Raw(429, "slow down".to_string())
            } else {
                Said::Result(receipt(status))
            }
        }
        _ => stand_in::unknown_method(),
    });
    let url = stand_in::answering(Some(31337), answer).await;
    chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap()
}

/// What a send came back with, given it a few seconds at most: the receipt
/// is there to be read from the second look on.
async fn within_seconds<T>(
    send: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    tokio::time::timeout(Duration::from_secs(20), send)
        .await
        .expect("the receipt was there to be read")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_failed_receipt_read_does_not_end_the_wait() {
    let from = Address::repeat_byte(0x01);
    let to = Address::repeat_byte(0x44);

    let sent = within_seconds(client(1, true).await.transfer(from, to, U256::from(1u64))).await;
    assert!(
        sent.is_ok(),
        "the transfer landed, but: {:#}",
        sent.unwrap_err()
    );

    let call = IERC20::approveCall {
        spender: to,
        value: U256::from(1u64),
    };
    let sent = within_seconds(client(1, true).await.send(from, to, call)).await;
    assert!(
        sent.is_ok(),
        "the call landed, but: {:#}",
        sent.unwrap_err()
    );

    let raw = Bytes::from(vec![0u8; 4]);
    let sent = within_seconds(client(1, true).await.send_raw(from, to, raw)).await;
    assert!(
        sent.is_ok(),
        "the raw send landed, but: {:#}",
        sent.unwrap_err()
    );

    let deployed = within_seconds(client(1, true).await.deploy(from, vec![0x60, 0x00])).await;
    assert!(
        deployed.is_ok(),
        "the deployment landed, but: {:#}",
        deployed.unwrap_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reverted_deployment_is_an_error() {
    let client = client(0, false).await;
    let deployed =
        within_seconds(client.deploy(Address::repeat_byte(0x01), vec![0x60, 0x00, 0xfd])).await;
    let Err(error) = deployed else {
        panic!(
            "a reverted creation came back as deployed at {:?}",
            deployed.unwrap()
        );
    };
    assert!(
        error.chain().any(|cause| cause.is::<chain::Reverted>()),
        "{error:#}"
    );
}
