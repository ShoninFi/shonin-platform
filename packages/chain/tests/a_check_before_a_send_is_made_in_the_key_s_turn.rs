//! A key sends one transaction at a time. `send_if` makes its check once the
//! key's turn has come, so none of the key's sends is being filled in or
//! waited on between the check and the send it allows; and a send's gas may
//! still be added for as long
//! as its receipt is watched for, which the key says. Needs the `args`
//! feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

async fn key_on(node: &Arc<std::sync::Mutex<stand_in::Node>>) -> chain::KeyClient {
    let url = stand_in::start(node.clone()).await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    client.with_key(PrivateKeySigner::random()).unwrap()
}

async fn until(what: &str, done: impl Fn() -> bool) {
    for _ in 0..200 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("never happened: {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_check_waits_for_the_key_s_turn_and_a_check_that_says_no_sends_nothing() {
    let node = stand_in::node(31337);
    let key = key_on(&node).await;

    // A send whose receipt has not come holds the key's turn.
    let first = {
        let key = key.clone();
        tokio::spawn(async move { key.send(Address::repeat_byte(0x22), approve()).await })
    };
    until("the first send", || node.lock().unwrap().sent.len() == 1).await;

    let checked = Arc::new(AtomicBool::new(false));
    let second = {
        let key = key.clone();
        let checked = checked.clone();
        tokio::spawn(async move {
            key.send_if(
                async move {
                    checked.store(true, Ordering::SeqCst);
                    Ok(false)
                },
                Address::repeat_byte(0x22),
                approve(),
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !checked.load(Ordering::SeqCst),
        "the check was made while another send held the key's turn"
    );

    // The first lands and gives the turn up: the check is made, says no,
    // and nothing more is sent.
    node.lock().unwrap().mined = true;
    first.await.unwrap().unwrap();
    let sent = second.await.unwrap().unwrap();
    assert!(checked.load(Ordering::SeqCst));
    assert!(sent.is_none());
    assert_eq!(
        node.lock().unwrap().sent.len(),
        1,
        "a check that said no sent"
    );

    // One that says yes sends.
    let sent = key
        .send_if(async { Ok(true) }, Address::repeat_byte(0x22), approve())
        .await
        .unwrap();
    assert!(sent.is_some());
    assert_eq!(node.lock().unwrap().sent.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_is_in_flight_until_it_lands_though_its_sender_stopped_waiting() {
    let node = stand_in::node(31337);
    let key = key_on(&node).await;
    assert!(!key.sends_in_flight());
    assert!(key.last_settled().is_none());

    let sending = {
        let key = key.clone();
        tokio::spawn(async move { key.send(Address::repeat_byte(0x22), approve()).await })
    };
    until("the send", || node.lock().unwrap().sent.len() == 1).await;
    sending.abort();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        key.sends_in_flight(),
        "a send whose receipt is still watched for was said to be settled"
    );

    let before = std::time::Instant::now();
    node.lock().unwrap().mined = true;
    until("the send to land", || !key.sends_in_flight()).await;
    assert!(key.gas_spent() > 0, "its gas was not counted");
    assert!(key.last_settled().is_some_and(|at| at >= before));
    // The platform's own count is apart from it.
    assert!(!key.unmetered().sends_in_flight());
}
