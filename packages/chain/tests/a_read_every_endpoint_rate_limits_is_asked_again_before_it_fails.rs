//! A read every endpoint of a chain refused for the rate of requests is
//! asked again a few times, with growing pauses, before it fails.
//!
//! Free public endpoints each limit how fast one address may ask, and a whole
//! stack on one machine can pass every one of a chain's limits at once for a
//! moment. A read that then fails at once fails whatever was waiting on it (a
//! seed, a round of the keeper) for a limit that lifts within seconds. So a
//! read whose every endpoint limited the rate (and nothing else went wrong)
//! waits and asks again, a bounded number of times: a head read, a contract
//! read, a read pinned to a block and a log read alike. A send is never asked
//! again this way: one that every endpoint refused for the rate was never
//! sent, and is reported so.
//!
//! Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alloy::eips::BlockId;
use alloy::primitives::{Address, B256, U256};
use alloy::rpc::types::Filter;
use alloy::signers::local::PrivateKeySigner;
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::json;
use stand_in::Said;

/// How often each method was asked, by method.
type Asked = Arc<Mutex<HashMap<String, u32>>>;

/// An endpoint that answers `eth_chainId` and the connect-time check, and
/// refuses every other request of `limited` with an HTTP 429 and an empty
/// body its first `refusals` times (`u32::MAX`: always).
fn endpoint(limited: &'static [&'static str], refusals: u32, asked: Asked) -> stand_in::Answer {
    Arc::new(move |method, _| {
        let count = {
            let mut asked = asked.lock().unwrap();
            let count = asked.entry(method.to_string()).or_default();
            *count += 1;
            *count
        };
        if limited.contains(&method) && count <= refusals {
            return Said::Raw(429, String::new());
        }
        match method {
            "eth_blockNumber" => Said::Result(json!("0x10")),
            "eth_call" => Said::Result(json!(B256::from(U256::from(7u64)))),
            "eth_getLogs" => Said::Result(json!([])),
            "eth_getBlockByNumber" => {
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.number = 16;
                block.header.inner.timestamp = 1_700_000_000;
                block.header.inner.base_fee_per_gas = Some(1_000_000_000);
                Said::Result(serde_json::to_value(block).unwrap())
            }
            "eth_gasPrice" | "eth_maxPriorityFeePerGas" => Said::Result(json!("0x3b9aca00")),
            "eth_feeHistory" => Said::Result(json!({
                "oldestBlock": "0x1",
                "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                "gasUsedRatio": [0.5],
                "reward": [["0x3b9aca00"]],
            })),
            "eth_estimateGas" => Said::Result(json!("0x5208")),
            "eth_getTransactionCount" => Said::Result(json!("0x0")),
            "eth_getTransactionReceipt" => Said::Result(serde_json::Value::Null),
            _ => stand_in::unknown_method(),
        }
    })
}

async fn client_over(
    count: usize,
    limited: &'static [&'static str],
    refusals: u32,
) -> (chain::Client, Vec<Asked>) {
    let mut urls = Vec::new();
    let mut asked = Vec::new();
    for _ in 0..count {
        let each: Asked = Default::default();
        urls.push(
            stand_in::answering(Some(31337), endpoint(limited, refusals, each.clone())).await,
        );
        asked.push(each);
    }
    let client = chain::Client::connect(&stand_in::config(&urls.join(" ")), Signing::ReadOnly)
        .await
        .unwrap();
    (client, asked)
}

fn asked(asked: &[Asked], method: &str) -> u32 {
    asked
        .iter()
        .map(|each| each.lock().unwrap().get(method).copied().unwrap_or(0))
        .sum()
}

fn balance_of() -> IERC20::balanceOfCall {
    IERC20::balanceOfCall {
        account: Address::ZERO,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_every_endpoint_rate_limits_twice_are_answered() {
    // One endpoint, and two behind one name: each limits the rate of the
    // read twice, then answers.
    for count in [1, 2] {
        let (client, _) = client_over(count, &["eth_call"], 2).await;
        let value = client
            .read(Address::repeat_byte(0x22), balance_of())
            .await
            .expect("a contract read whose endpoints lifted their limits is answered");
        assert_eq!(value, U256::from(7u64));

        // The connect-time check asks each endpoint its head once first.
        let (client, _) = client_over(count, &["eth_blockNumber"], 3).await;
        assert_eq!(client.block_number().await.unwrap(), 16);
    }
    let (client, _) = client_over(2, &["eth_call"], 2).await;
    let value = client
        .read_at(Address::repeat_byte(0x22), balance_of(), BlockId::number(5))
        .await
        .expect("a pinned read whose endpoints lifted their limits is answered");
    assert_eq!(value, U256::from(7u64));

    let (client, _) = client_over(2, &["eth_getLogs"], 2).await;
    let logs = client
        .logs(&Filter::new().from_block(1).to_block(10))
        .await
        .expect("a log read whose endpoints lifted their limits is answered");
    assert!(logs.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_every_endpoint_always_rate_limits_fail_after_a_few_more_tries() {
    let (client, endpoints) = client_over(2, &["eth_call"], u32::MAX).await;
    client
        .read(Address::repeat_byte(0x22), balance_of())
        .await
        .expect_err("every endpoint always limits the rate");
    let tries = asked(&endpoints, "eth_call");
    assert!(
        (3..=8).contains(&tries),
        "asked {tries} times over two endpoints: not again, or without bound"
    );

    let (client, endpoints) = client_over(2, &["eth_call"], u32::MAX).await;
    client
        .read_at(Address::repeat_byte(0x22), balance_of(), BlockId::number(5))
        .await
        .expect_err("every endpoint always limits the rate");
    assert_eq!(asked(&endpoints, "eth_call"), 2 * 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_every_endpoint_rate_limits_is_not_sent_again() {
    let (client, endpoints) = client_over(2, &["eth_sendRawTransaction"], u32::MAX).await;
    let key = client.with_key(PrivateKeySigner::random()).unwrap();
    let error = key
        .send(
            Address::repeat_byte(0x22),
            IERC20::approveCall {
                spender: Address::repeat_byte(0x33),
                value: U256::from(1u64),
            },
        )
        .await
        .expect_err("every endpoint refused the send for the rate");
    assert!(
        !chain::may_still_land(&error),
        "a send no endpoint took is reported as never sent: {error:#}"
    );
    assert_eq!(
        asked(&endpoints, "eth_sendRawTransaction"),
        2,
        "the send was handed over again"
    );
}
