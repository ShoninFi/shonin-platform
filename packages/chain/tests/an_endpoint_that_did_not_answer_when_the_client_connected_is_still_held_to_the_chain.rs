//! An endpoint that didn't answer when the client connected is still held
//! to the chain before anything it says is taken as this chain's.
//!
//! Every endpoint of a list is asked its chain id at connect, and one on
//! another chain is refused: stations share one address on every chain, so
//! it would answer for that chain as this one. An endpoint that doesn't
//! answer then (it limits the rate while every service starts at once) is
//! left in the list. If it is never asked again, a mistyped endpoint for
//! another chain serves reads from the first moment the right one falters.
//!
//! Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alloy::primitives::{Address, B256, U256};
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::json;
use stand_in::Said;

/// What the contract at the shared address holds on this chain, and on the
/// other.
const HERE: u64 = 7;
const ON_THE_OTHER_CHAIN: u64 = 99;

fn word(value: u64) -> Said {
    Said::Result(json!(B256::from(U256::from(value))))
}

/// An endpoint on chain `chain_id` whose contract holds `value`. While
/// `limiting` is set it limits the rate of every request, its chain id
/// included.
fn endpoint(chain_id: u64, value: u64, limiting: Arc<AtomicBool>) -> stand_in::Answer {
    Arc::new(move |method, _request| {
        if limiting.load(Ordering::SeqCst) {
            return Said::Raw(429, "Too Many Requests".to_string());
        }
        match method {
            "eth_chainId" => Said::Result(json!(format!("0x{chain_id:x}"))),
            "eth_blockNumber" => Said::Result(json!("0x10")),
            "eth_call" => word(value),
            _ => stand_in::unknown_method(),
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_endpoint_on_another_chain_that_was_limiting_the_rate_at_connect_never_answers_for_this_one()
 {
    let ours_falters = Arc::new(AtomicBool::new(false));
    let other_starting = Arc::new(AtomicBool::new(true));
    let ours = stand_in::answering(None, endpoint(31337, HERE, ours_falters.clone())).await;
    let other = stand_in::answering(
        None,
        endpoint(8453, ON_THE_OTHER_CHAIN, other_starting.clone()),
    )
    .await;

    let connected = chain::Client::connect(
        &stand_in::config(&format!("{ours} {other}")),
        Signing::ReadOnly,
    )
    .await;
    // Refusing the list outright would be one way to hold it to the chain.
    let Ok(client) = connected else {
        return;
    };

    // The other endpoint answers again, and ours starts limiting the rate.
    other_starting.store(false, Ordering::SeqCst);
    ours_falters.store(true, Ordering::SeqCst);

    let read = client
        .read(
            Address::repeat_byte(0x22),
            IERC20::balanceOfCall {
                account: Address::ZERO,
            },
        )
        .await;
    assert_ne!(
        read.ok(),
        Some(U256::from(ON_THE_OTHER_CHAIN)),
        "a read for chain 31337 was answered by an endpoint on chain 8453"
    );
}
