//! Every wait for a transaction to land is bounded: a transaction the node
//! takes and never mines (dropped from its pool, a node restarted) comes back
//! as an unanswered send, never as a wait that holds up whatever sent it. The
//! executor stakes each vault's key with a transfer while it looks for work
//! on a chain, so one transfer that never lands must not stop it taking on
//! every other vault. Needs the `args` feature, as every service has it.
//!
//! The clock is paused once the transaction is out, so the waits run out at
//! once: nothing is ever mined here.

#![cfg(feature = "args")]

mod stand_in;

use std::future::Future;
use std::time::Duration;

use alloy::primitives::{Address, Bytes, U256};
use alloy::signers::local::PrivateKeySigner;
use chain::Signing;
use chain::bindings::erc20::IERC20;

/// Start a send against a node that never mines, and say how it ended, or
/// `None` if it was still waiting half an hour on.
async fn ends<T, F>(send: impl FnOnce(chain::Client, Address) -> F) -> Option<anyhow::Result<T>>
where
    F: Future<Output = anyhow::Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let node = stand_in::node(31337);
    let url = stand_in::start(node.clone()).await;
    // A client that signs for itself, as a testnet service does.
    let signer = PrivateKeySigner::random();
    let from = signer.address();
    let client = chain::Client::connect(&stand_in::config(&url), Signing::Keys(vec![signer]))
        .await
        .unwrap();
    let sending = tokio::spawn(send(client, from));
    while node.lock().unwrap().sent.is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::pause();
    let outcome = tokio::time::timeout(Duration::from_secs(30 * 60), sending)
        .await
        .ok()
        .map(|joined| joined.unwrap());
    tokio::time::resume();
    outcome
}

fn gave_up<T>(outcome: Option<anyhow::Result<T>>, what: &str) {
    let Some(result) = outcome else {
        panic!("{what} was still waiting for a receipt half an hour on");
    };
    let Err(error) = result else {
        panic!("{what} landed though nothing was mined");
    };
    assert!(chain::unanswered(&error), "{what}: {error:#}");
}

#[tokio::test]
async fn a_transfer_the_chain_never_mines_comes_back_unanswered() {
    let outcome = ends(|client, from| async move {
        client
            .transfer(from, Address::repeat_byte(0x44), U256::from(1u64))
            .await
    })
    .await;
    gave_up(outcome, "the transfer");
}

#[tokio::test]
async fn a_call_the_chain_never_mines_comes_back_unanswered() {
    let outcome = ends(|client, from| async move {
        client
            .send(
                from,
                Address::repeat_byte(0x22),
                IERC20::approveCall {
                    spender: Address::repeat_byte(0x33),
                    value: U256::from(1u64),
                },
            )
            .await
    })
    .await;
    gave_up(outcome, "the call");
}

#[tokio::test]
async fn raw_bytes_the_chain_never_mines_come_back_unanswered() {
    let outcome = ends(|client, from| async move {
        client
            .send_raw(from, Address::repeat_byte(0x22), Bytes::from(vec![0u8; 4]))
            .await
    })
    .await;
    gave_up(outcome, "the raw send");
}

#[tokio::test]
async fn a_deployment_the_chain_never_mines_comes_back_unanswered() {
    let outcome =
        ends(|client, from| async move { client.deploy(from, vec![0x60, 0x00]).await }).await;
    gave_up(outcome, "the deployment");
}
