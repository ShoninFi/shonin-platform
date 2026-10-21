//! A call or an estimate that reads HyperCore goes to an endpoint that
//! serves HyperCore's reads, and an endpoint's failure is never its answer.
//!
//! On HyperEVM a contract reads HyperCore through read precompiles, which
//! the node running the call answers by asking a HyperCore node of its own.
//! An endpoint whose node can't reach one fails every such call ("failed to
//! forward read precompile ...: client error (Connect)") while answering
//! every other read. Taken as the call's answer, a valuation whose estimate
//! reads a price through a precompile is never sent while that endpoint is
//! first in the list. So each endpoint is asked a precompile read when the
//! client connects; a call failed for a precompile it could not forward
//! moves on to the next endpoint and marks its target as one that reads
//! HyperCore; and such calls then go first to the endpoints that serve them.
//! An endpoint failing for its own reasons is passed over the same way; a
//! revert stays the answer.
//!
//! Needs the `args` feature.

#![cfg(feature = "args")]

mod stand_in;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::signers::local::PrivateKeySigner;
use chain::Signing;
use chain::bindings::erc20::IERC20;
use serde_json::{Value, json};
use stand_in::Said;

/// HyperEVM's testnet.
const CHAIN: u64 = 998;

/// A contract whose calls read a HyperCore precompile, as a vault valuing a
/// position on HyperCore does.
const VALUED: Address = Address::repeat_byte(0x7a);

/// The gas an endpoint that serves HyperCore's reads estimates for a call to
/// [`VALUED`].
const ESTIMATE: u64 = 503_375;

fn call() -> IERC20::approveCall {
    IERC20::approveCall {
        spender: Address::repeat_byte(0x33),
        value: U256::from(1u64),
    }
}

/// How each endpoint answers a call or estimate to [`VALUED`] or to a
/// precompile.
#[derive(Clone, Copy)]
enum Reads {
    /// It fails them for a precompile it could not forward.
    CannotForward,
    /// It answers them.
    Serves,
    /// It answers precompile reads and estimates, and fails the first call
    /// to [`VALUED`] as its own trouble (its node unreachable), then answers.
    FailsOnce,
    /// It answers precompile reads, and reverts every call to [`VALUED`].
    Reverts,
}

/// What each endpoint was asked: method and target, in order.
type Asked = Arc<Mutex<Vec<(String, Option<Address>)>>>;

fn refusal(request: &Value, code: i64, message: &str) -> Said {
    Said::Raw(
        200,
        json!({
            "jsonrpc": "2.0",
            "id": request["id"].clone(),
            "error": {"code": code, "message": message},
        })
        .to_string(),
    )
}

fn receipt(hash: &Value) -> Value {
    json!({
        "transactionHash": hash,
        "transactionIndex": "0x0",
        "blockHash": B256::repeat_byte(0xbb),
        "blockNumber": "0x11",
        "from": Address::ZERO,
        "to": VALUED,
        "cumulativeGasUsed": "0x5208",
        "gasUsed": "0x5208",
        "effectiveGasPrice": "0x3b9aca00",
        "contractAddress": null,
        "logs": [],
        "logsBloom": format!("0x{}", "00".repeat(256)),
        "status": "0x1",
        "type": "0x2",
    })
}

/// The transactions sent through any endpoint, by hash, with their gas.
type Mined = Arc<Mutex<HashMap<B256, u64>>>;

fn endpoint(reads: Reads, asked: Asked, mined: Mined) -> stand_in::Answer {
    let failed_once = Arc::new(Mutex::new(false));
    Arc::new(move |method, request| {
        if let Some(said) = stand_in::past_block_check(method, request, stand_in::Past::Honours) {
            return said;
        }
        let to: Option<Address> = request["params"][0]["to"]
            .as_str()
            .and_then(|to| to.parse().ok());
        asked.lock().unwrap().push((method.to_string(), to));
        let precompile = to.is_some_and(|to| to[..18] == [0; 18] && to[18] == 0x08);
        let reads_hypercore = precompile || to == Some(VALUED);
        if matches!(method, "eth_call" | "eth_estimateGas") && reads_hypercore {
            match reads {
                Reads::CannotForward => {
                    return refusal(
                        request,
                        -32603,
                        "Revm error: failed to forward read precompile \
                         0x000000000000000000000000000000000000080F: client error (Connect)",
                    );
                }
                Reads::FailsOnce
                    if method == "eth_call" && !precompile && !*failed_once.lock().unwrap() =>
                {
                    *failed_once.lock().unwrap() = true;
                    return refusal(
                        request,
                        -32603,
                        "dial tcp 10.0.0.2:3001: connection refused",
                    );
                }
                Reads::Reverts if !precompile => {
                    return refusal(request, 3, "execution reverted");
                }
                _ => {}
            }
        }
        match method {
            "eth_call" => Said::Result(json!(B256::from(U256::from(7u64)))),
            "eth_estimateGas" if to == Some(VALUED) => {
                Said::Result(json!(format!("0x{ESTIMATE:x}")))
            }
            "eth_estimateGas" => Said::Result(json!("0x5208")),
            "eth_getBlockByNumber" => {
                let mut block =
                    alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
                block.header.inner.number = 0x40;
                block.header.inner.timestamp = 1_700_000_000;
                block.header.inner.base_fee_per_gas = Some(1_000_000_000);
                Said::Result(serde_json::to_value(block).unwrap())
            }
            "eth_blockNumber" => Said::Result(json!("0x40")),
            "eth_gasPrice" | "eth_maxPriorityFeePerGas" => Said::Result(json!("0x3b9aca00")),
            "eth_feeHistory" => Said::Result(json!({
                "oldestBlock": "0x1",
                "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                "gasUsedRatio": [0.5],
                "reward": [["0x3b9aca00"]],
            })),
            "eth_sendRawTransaction" => {
                use alloy::consensus::Transaction;
                use alloy::eips::eip2718::Decodable2718;
                let raw = alloy::hex::decode(request["params"][0].as_str().unwrap()).unwrap();
                let sent = alloy::consensus::TxEnvelope::decode_2718(&mut raw.as_slice()).unwrap();
                mined
                    .lock()
                    .unwrap()
                    .insert(keccak256(&raw), sent.gas_limit());
                Said::Result(json!(keccak256(&raw)))
            }
            "eth_getTransactionReceipt" => {
                let asked: B256 = request["params"][0].as_str().unwrap().parse().unwrap();
                if mined.lock().unwrap().contains_key(&asked) {
                    Said::Result(receipt(&request["params"][0]))
                } else {
                    Said::Result(Value::Null)
                }
            }
            _ => stand_in::unknown_method(),
        }
    })
}

/// A client over one endpoint per entry of `reads`, in that order, signing
/// with a key of its own; what each was asked; and what was sent.
async fn client_over(reads: &[Reads]) -> (chain::Client, Address, Vec<Asked>, Mined) {
    let mined = Mined::default();
    let mut urls = Vec::new();
    let mut asked = Vec::new();
    for reads in reads {
        let seen = Asked::default();
        urls.push(
            stand_in::answering(Some(CHAIN), endpoint(*reads, seen.clone(), mined.clone())).await,
        );
        asked.push(seen);
    }
    let signer = PrivateKeySigner::random();
    let from = signer.address();
    let client = chain::Client::connect(
        &stand_in::config(&urls.join(" ")),
        Signing::Keys(vec![signer]),
    )
    .await
    .unwrap();
    (client, from, asked, mined)
}

/// How many times `asked` saw `method` sent to [`VALUED`].
fn count(asked: &Asked, method: &str) -> usize {
    asked
        .lock()
        .unwrap()
        .iter()
        .filter(|(seen, to)| seen == method && *to == Some(VALUED))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_valuation_s_estimate_and_send_go_through_the_endpoint_that_serves_hypercore_reads() {
    let (client, from, asked, mined) = client_over(&[Reads::CannotForward, Reads::Serves]).await;

    let receipt = client.send_estimated(from, VALUED, call()).await;
    let receipt = match receipt {
        Ok(receipt) => receipt,
        Err(error) => panic!(
            "an endpoint of the list serves HyperCore's reads, and the estimate failed on the one \
             that does not: {error:#}"
        ),
    };
    assert!(
        count(&asked[0], "eth_estimateGas") >= 1,
        "the endpoint that cannot forward precompile reads was never asked, so the test proves \
         nothing"
    );
    let gas = mined
        .lock()
        .unwrap()
        .get(&receipt.transaction_hash)
        .copied();
    assert_eq!(
        gas,
        Some(ESTIMATE),
        "sent with another gas limit than the estimate"
    );

    // The target is now known to read HyperCore: the next estimates go
    // straight to the endpoint that serves them, and the other is not asked.
    let before = count(&asked[0], "eth_estimateGas");
    for _ in 0..3 {
        client.send_estimated(from, VALUED, call()).await.unwrap();
    }
    assert_eq!(count(&asked[0], "eth_estimateGas"), before);
    assert_eq!(count(&asked[1], "eth_estimateGas"), 4);

    // An observation or a simulation that reads it is made on that endpoint
    // alone; one that reads nothing of HyperCore on any.
    assert_eq!(client.hypercore_order([VALUED]), Some(vec![1]));
    assert_eq!(client.hypercore_order([Address::repeat_byte(0x01)]), None);

    // A read of it goes the same way.
    let read = client
        .read(VALUED, IERC20::balanceOfCall { account: from })
        .await;
    assert_eq!(read.unwrap(), U256::from(7u64));
    assert_eq!(count(&asked[0], "eth_call"), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_endpoint_that_fails_a_hypercore_read_for_its_own_trouble_is_passed_for_the_next() {
    let (client, from, asked, _) =
        client_over(&[Reads::CannotForward, Reads::FailsOnce, Reads::Serves]).await;

    // Learned through the first endpoint, taken by the second.
    client.send_estimated(from, VALUED, call()).await.unwrap();
    assert_eq!(count(&asked[2], "eth_estimateGas"), 0);

    // The second endpoint's node is now unreachable once: the read goes on
    // to the third.
    let read = client
        .read(VALUED, IERC20::balanceOfCall { account: from })
        .await;
    match read {
        Ok(value) => assert_eq!(value, U256::from(7u64)),
        Err(error) => panic!("an endpoint's own failure was taken as the read's: {error:#}"),
    }
    assert_eq!(count(&asked[1], "eth_call"), 1);
    assert_eq!(count(&asked[2], "eth_call"), 1);
    assert_eq!(count(&asked[0], "eth_call"), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revert_from_an_endpoint_that_serves_hypercore_reads_is_the_answer() {
    let (client, from, asked, mined) =
        client_over(&[Reads::CannotForward, Reads::Reverts, Reads::Serves]).await;

    for _ in 0..2 {
        let refused = client
            .send_estimated(from, VALUED, call())
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
    }
    assert_eq!(count(&asked[1], "eth_estimateGas"), 2);
    assert_eq!(
        count(&asked[2], "eth_estimateGas"),
        0,
        "a revert was taken for an endpoint's trouble"
    );
    assert!(mined.lock().unwrap().is_empty(), "a reverted call was sent");
}
