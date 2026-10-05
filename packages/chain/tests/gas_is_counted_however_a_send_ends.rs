//! A key's gas counter is what the agent's bill is made from, so a
//! transaction's gas is counted once it lands, whether or not whoever sent it
//! was still waiting. Needs the `args` feature, as every service has it.
//!
//! The clock is paused only to wait out the sender's timeout: a paused clock
//! jumps ahead whenever the runtime is idle, which would also overtake the
//! stand-in node's answers.

#![cfg(feature = "args")]

mod stand_in;

use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::signers::local::PrivateKeySigner;
use chain::Signing;
use chain::bindings::erc20::IERC20;

fn approve() -> IERC20::approveCall {
    IERC20::approveCall {
        spender: Address::repeat_byte(0x33),
        value: U256::from(1u64),
    }
}

/// Wait a few seconds at most until the key has counted some gas.
async fn counted(key: &chain::KeyClient) -> u128 {
    for _ in 0..500 {
        if key.gas_spent() > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    key.gas_spent()
}

/// Wait until the stand-in node has been sent a transaction.
async fn broadcast(node: &std::sync::Mutex<stand_in::Node>) {
    while node.lock().unwrap().sent.is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn gas_of_a_transaction_that_lands_after_the_timeout_is_counted() {
    let node = stand_in::node(31337);
    let url = stand_in::start(node.clone()).await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    let key = client.with_key(PrivateKeySigner::random()).unwrap();

    let sending = {
        let key = key.clone();
        tokio::spawn(async move { key.send(Address::repeat_byte(0x22), approve()).await })
    };
    broadcast(&node).await;
    // Nothing is mined, so the clock runs on to the sender's timeout.
    tokio::time::pause();
    let error = sending
        .await
        .unwrap()
        .expect_err("the receipt never came within the wait");
    tokio::time::resume();
    assert!(chain::unanswered(&error), "{error:#}");
    assert_eq!(node.lock().unwrap().sent.len(), 1, "it was broadcast once");

    // The chain mines it after the sender gave up on it.
    node.lock().unwrap().mined = true;
    assert_eq!(counted(&key).await, 21_000 * 1_000_000_000);
}

#[tokio::test]
async fn gas_of_a_send_whose_reply_never_came_is_counted_when_it_lands() {
    let node = stand_in::node(31337);
    // The node takes the transaction, and its reply never makes it back:
    // the hash it would have named is known only to whoever signed it.
    node.lock().unwrap().send_delay = Some(Duration::from_secs(3_600));
    let url = stand_in::start(node.clone()).await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    let key = client.with_key(PrivateKeySigner::random()).unwrap();

    let sending = {
        let key = key.clone();
        tokio::spawn(async move { key.send(Address::repeat_byte(0x22), approve()).await })
    };
    broadcast(&node).await;
    tokio::time::pause();
    let error = sending.await.unwrap().expect_err("no reply came");
    tokio::time::resume();
    assert!(chain::unanswered(&error), "{error:#}");

    // It was in the node all along, and is mined.
    node.lock().unwrap().mined = true;
    assert_eq!(counted(&key).await, 21_000 * 1_000_000_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gas_of_a_send_whose_caller_was_stopped_is_counted() {
    let node = stand_in::node(31337);
    let url = stand_in::start(node.clone()).await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    let key = client.with_key(PrivateKeySigner::random()).unwrap();

    // The caller is stopped once the transaction is out, the way an agent
    // is stopped mid-send.
    let sending = {
        let key = key.clone();
        tokio::spawn(async move { key.send(Address::repeat_byte(0x22), approve()).await })
    };
    broadcast(&node).await;
    sending.abort();

    node.lock().unwrap().mined = true;
    assert_eq!(counted(&key).await, 21_000 * 1_000_000_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gas_of_a_send_whose_caller_was_stopped_while_the_node_was_answering_is_counted() {
    let node = stand_in::node(31337);
    // The node takes the transaction, then takes its time to say so.
    node.lock().unwrap().send_delay = Some(Duration::from_millis(500));
    let url = stand_in::start(node.clone()).await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    let key = client.with_key(PrivateKeySigner::random()).unwrap();

    let sending = {
        let key = key.clone();
        tokio::spawn(async move { key.send(Address::repeat_byte(0x22), approve()).await })
    };
    // The node has the transaction and has not answered yet when the caller
    // is stopped.
    broadcast(&node).await;
    sending.abort();
    let _ = sending.await;

    node.lock().unwrap().mined = true;
    assert_eq!(counted(&key).await, 21_000 * 1_000_000_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gas_counted_by_an_unmetered_clone_is_its_own() {
    let node = stand_in::node(31337);
    node.lock().unwrap().mined = true;
    let url = stand_in::start(node.clone()).await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    let key = client.with_key(PrivateKeySigner::random()).unwrap();
    let platform = key.unmetered();

    platform
        .send(Address::repeat_byte(0x22), approve())
        .await
        .unwrap();
    assert_eq!(platform.gas_spent(), 21_000 * 1_000_000_000);
    assert_eq!(key.gas_spent(), 0, "the agent's count is untouched");
}
