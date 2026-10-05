//! A key sends one transaction at a time, and its turn is held by the task
//! that broadcasts. A node that takes a transaction and never says so must
//! not hold that turn for good: once the sender has given up, the key's next
//! transaction (its own, or its unmetered clone's, such as a blessing) goes
//! out. Needs the `args` feature, as every service has it.

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

/// Wait until the stand-in node has been sent `count` transactions, or give
/// up after a while of (paused) time.
async fn broadcast_count(node: &std::sync::Mutex<stand_in::Node>, count: usize) -> usize {
    for _ in 0..400 {
        let sent = node.lock().unwrap().sent.len();
        if sent >= count {
            return sent;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    node.lock().unwrap().sent.len()
}

#[tokio::test]
async fn the_next_send_is_broadcast_once_a_send_the_node_never_acknowledged_is_given_up_on() {
    let node = stand_in::node(31337);
    // The node takes the transaction and then never says so.
    node.lock().unwrap().send_delay = Some(Duration::from_secs(3_600));
    let url = stand_in::start(node.clone()).await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    let key = client.with_key(PrivateKeySigner::random()).unwrap();

    let first = {
        let key = key.clone();
        tokio::spawn(async move { key.send(Address::repeat_byte(0x22), approve()).await })
    };
    assert_eq!(
        broadcast_count(&node, 1).await,
        1,
        "the first was broadcast"
    );
    // Nothing comes back, so the clock runs on to the sender's timeout.
    tokio::time::pause();
    let error = first
        .await
        .unwrap()
        .expect_err("the sender gave up on the first send");
    assert!(chain::unanswered(&error), "{error:#}");
    // Back to real time, so the wait below is a real wait rather than a
    // paused clock jumping past a send still on its way to the node.
    tokio::time::resume();

    // The sender has been told the first never landed. The node now answers
    // sends at once again: the key's next transaction should go out.
    node.lock().unwrap().send_delay = None;
    let platform = key.unmetered();
    let _second =
        tokio::spawn(async move { platform.send(Address::repeat_byte(0x22), approve()).await });

    assert_eq!(
        broadcast_count(&node, 2).await,
        2,
        "the second send is still waiting for the key's turn"
    );
}
