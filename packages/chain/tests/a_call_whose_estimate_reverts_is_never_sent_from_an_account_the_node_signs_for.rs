//! A node that signs for its accounts fills a send's missing gas limit in
//! itself; when the call's estimate reverts, a local node fills in the
//! block's whole gas limit and sends it anyway, and the call lands as a
//! revert holding a whole block's room. Sent with its estimate asked first,
//! a call the chain would revert is refused before anything goes out, and
//! one it takes goes out with its estimate, far under the block's limit.

mod stand_in_node;

use alloy::primitives::{Address, U256};
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::json;
use stand_in_node::{Anvil, config};

fn number(value: &serde_json::Value) -> u64 {
    u64::from_str_radix(value.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

#[tokio::test]
async fn a_call_whose_estimate_reverts_is_never_sent_from_an_account_the_node_signs_for() {
    let anvil = Anvil::start(31_420_101, 1_700_000_000).await;
    anvil.rpc("evm_setAutomine", json!([true])).await.unwrap();
    // A contract that reverts whatever it is asked, and one that takes it.
    let refuses = Address::repeat_byte(0xaa);
    let takes = Address::repeat_byte(0xbb);
    anvil
        .rpc("anvil_setCode", json!([refuses, "0x60006000fd"]))
        .await
        .unwrap();
    anvil
        .rpc("anvil_setCode", json!([takes, "0x00"]))
        .await
        .unwrap();
    let client = chain::Client::connect(&config(&[&anvil.url]), Signing::Unlocked)
        .await
        .unwrap();
    let from = client.accounts[0];
    let call = || IERC20::approveCall {
        spender: from,
        value: U256::from(1u64),
    };
    let sends_of = async || {
        number(
            &anvil
                .rpc("eth_getTransactionCount", json!([from, "latest"]))
                .await
                .unwrap(),
        )
    };
    let head = async || number(&anvil.rpc("eth_blockNumber", json!([])).await.unwrap());
    let (sends_before, head_before) = (sends_of().await, head().await);

    let refused = client
        .send_estimated(from, refuses, call())
        .await
        .expect_err("a call the chain reverts was taken");
    assert!(
        refused.chain().any(|cause| cause.is::<chain::NeverSent>()),
        "{refused:#}"
    );
    assert!(
        format!("{refused:#}").contains("revert"),
        "the refusal is not the estimate's revert: {refused:#}"
    );
    assert_eq!(sends_of().await, sends_before, "a refused call went out");
    assert_eq!(head().await, head_before, "a block was mined for it");

    let receipt = client.send_estimated(from, takes, call()).await.unwrap();
    let sent = anvil
        .rpc(
            "eth_getTransactionByHash",
            json!([receipt.transaction_hash]),
        )
        .await
        .unwrap();
    let block = anvil
        .rpc("eth_getBlockByNumber", json!(["latest", false]))
        .await
        .unwrap();
    let (gas, block_limit) = (number(&sent["gas"]), number(&block["gasLimit"]));
    assert!(
        gas < 100_000 && gas < block_limit,
        "sent with {gas} gas, the block's limit being {block_limit}"
    );
}
