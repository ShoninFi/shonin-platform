//! A renewal whose blessing landed is followed, however `bless` reports.
//!
//! `keep_blessed` runs its `on_renewal` hook whenever the key's blessing is
//! seen to have moved. A renewal can land on the chain and still fail after
//! the send (its receipt or the read-back lost), and what follows the
//! blessing (the venue's approval of the key) has to follow it all the same,
//! or the venue turns the key's orders away from the old blessing's end.
//!
//! Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol_types::SolCall;
use chain::Signing;
use chain::bindings::attestation::{IAttestationRegistry, ImageKind, KeyInfo};
use serde_json::{Value, json};
use stand_in::Said;

const NOW: u64 = 1_700_000_000;
const TTL: u64 = 72 * 3_600;

struct Registry {
    /// The key's `blessedUntil`.
    blessed_until: u64,
    /// Blessings sent.
    blessings: usize,
    /// Whether the next read of the key fails, as a gateway's hiccup would.
    next_read_fails: bool,
    /// Whether the read-back right after a blessing fails.
    read_back_fails: bool,
    sent: Vec<B256>,
}

fn deployments() -> chain::Deployments {
    let zero = json!(format!("{:?}", Address::ZERO));
    let mut deployments = json!({
        "chain_key": "evm:31337", "chain_id": 31337, "rpc_url": "", "deployed_block": 0,
    });
    deployments["roles"] = [
        "admin", "platform", "attestor", "builder", "reviewer", "executor", "keeper", "operator",
        "author", "viewer",
    ]
    .iter()
    .map(|role| (role.to_string(), zero.clone()))
    .collect::<serde_json::Map<_, _>>()
    .into();
    for name in [
        "usd",
        "venue",
        "attestation_verifier",
        "attestation",
        "publishing",
        "prices",
        "strategies",
        "reviews",
        "profiles",
        "factory",
        "station_factory",
        "driver",
        "bridge",
    ] {
        deployments[name] = zero.clone();
    }
    serde_json::from_value(deployments).unwrap()
}

fn answer(registry: &Mutex<Registry>, method: &str, body: &Value) -> Said {
    let mut registry = registry.lock().unwrap();
    match method {
        "eth_blockNumber" => Said::Result(json!("0x10")),
        "eth_getBlockByNumber" => {
            let mut block = alloy::rpc::types::Block::<alloy::rpc::types::Transaction>::default();
            block.header.inner.number = 16;
            block.header.inner.timestamp = NOW;
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
        "eth_estimateGas" => Said::Result(json!("0x30000")),
        "eth_getTransactionCount" => Said::Result(json!(format!("0x{:x}", registry.sent.len()))),
        // The attestor's seal, as a development chain's node gives it.
        "eth_sign" => Said::Result(json!(Bytes::from(vec![0x11u8; 65]))),
        "eth_sendRawTransaction" => {
            let raw = alloy::hex::decode(body["params"][0].as_str().unwrap()).unwrap();
            let hash = alloy::primitives::keccak256(&raw);
            registry.sent.push(hash);
            // The only thing the key sends here is its blessing.
            registry.blessings += 1;
            registry.blessed_until = NOW + TTL;
            // The read-back right after it fails once, if the test says so.
            registry.next_read_fails = registry.read_back_fails;
            Said::Result(json!(hash))
        }
        "eth_getTransactionReceipt" => {
            let asked: B256 = body["params"][0].as_str().unwrap().parse().unwrap();
            if !registry.sent.contains(&asked) {
                return Said::Result(Value::Null);
            }
            Said::Result(json!({
                "transactionHash": asked,
                "transactionIndex": "0x0",
                "blockHash": B256::repeat_byte(0xbb),
                "blockNumber": "0x11",
                "from": Address::ZERO,
                "to": Address::ZERO,
                "cumulativeGasUsed": "0x5208",
                "gasUsed": "0x5208",
                "effectiveGasPrice": "0x3b9aca00",
                "contractAddress": null,
                "logs": [],
                "logsBloom": format!("0x{}", "00".repeat(256)),
                "status": "0x1",
                "type": "0x2",
            }))
        }
        "eth_call" => {
            let tx = &body["params"][0];
            let data = tx["input"]
                .as_str()
                .or_else(|| tx["data"].as_str())
                .unwrap();
            let input = alloy::hex::decode(data).unwrap();
            let selector: [u8; 4] = input[..4].try_into().unwrap();
            let out = match selector {
                IAttestationRegistry::blessingTtlCall::SELECTOR => {
                    IAttestationRegistry::blessingTtlCall::abi_encode_returns(&TTL)
                }
                IAttestationRegistry::isImageAllowedCall::SELECTOR => {
                    IAttestationRegistry::isImageAllowedCall::abi_encode_returns(&true)
                }
                IAttestationRegistry::keyInfoCall::SELECTOR => {
                    if registry.next_read_fails {
                        registry.next_read_fails = false;
                        return Said::Raw(503, "upstream busy".to_string());
                    }
                    IAttestationRegistry::keyInfoCall::abi_encode_returns(&KeyInfo {
                        imageDigest: B256::ZERO,
                        projectId: B256::ZERO,
                        kind: ImageKind::Executor,
                        strategyHash: B256::repeat_byte(0x42),
                        vaultChain: U256::from(31337u64),
                        vault: Address::repeat_byte(0x11),
                        trustRoot: B256::ZERO,
                        verifier: Address::ZERO,
                        blessedUntil: registry.blessed_until,
                    })
                }
                // The registry counts the key as blessed while its record
                // runs.
                IAttestationRegistry::isBlessedForCall::SELECTOR => {
                    IAttestationRegistry::isBlessedForCall::abi_encode_returns(
                        &(registry.blessed_until >= NOW),
                    )
                }
                _ => return stand_in::unknown_method(),
            };
            Said::Result(json!(alloy::hex::encode_prefixed(out)))
        }
        _ => stand_in::unknown_method(),
    }
}

/// Let `keep_blessed` renew a blessing with a quarter left, then look twice
/// more, and say how many blessings were sent, where the blessing ends, and
/// how many times `on_renewal` ran.
async fn renew(read_back_fails: bool) -> (usize, u64, usize) {
    // A quarter of the blessing is left: a renewal is due.
    let registry = Arc::new(Mutex::new(Registry {
        blessed_until: NOW + TTL / 4,
        blessings: 0,
        next_read_fails: false,
        read_back_fails,
        sent: Vec::new(),
    }));
    let answering = registry.clone();
    let url = stand_in::answering(
        Some(31337),
        Arc::new(move |method, body| answer(&answering, method, body)),
    )
    .await;
    let client = chain::Client::connect(&stand_in::config(&url), Signing::ReadOnly)
        .await
        .unwrap();
    let key = client.with_key(PrivateKeySigner::random()).unwrap();

    let renewals = Arc::new(AtomicUsize::new(0));
    let counted = renewals.clone();
    let renewing = chain::attest::keep_blessed(
        client.clone(),
        deployments(),
        "executor",
        key.address,
        chain::attest::Sender::Own(Box::new(key.unmetered())),
        Address::repeat_byte(0x11),
        B256::repeat_byte(0x42),
        move || {
            counted.fetch_add(1, Ordering::SeqCst);
        },
    );

    // The blessing goes out and lands; then two more looks, five seconds
    // apart, in which a missed renewal would be noticed.
    for _ in 0..300 {
        if registry.lock().unwrap().blessings > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_secs(11)).await;
    renewing.abort();

    let registry = registry.lock().unwrap();
    (
        registry.blessings,
        registry.blessed_until,
        renewals.load(Ordering::SeqCst),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_renewal_that_landed_is_followed_even_if_reading_it_back_failed() {
    let (blessings, until, renewals) = renew(true).await;
    assert_eq!(blessings, 1, "the renewal was sent once");
    assert_eq!(until, NOW + TTL, "the blessing was renewed");
    assert_eq!(
        renewals, 1,
        "a blessing renewed on the chain must be followed once, though reading it back failed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_renewal_read_back_at_once_is_followed_once() {
    let (blessings, until, renewals) = renew(false).await;
    assert_eq!(blessings, 1, "the renewal was sent once");
    assert_eq!(until, NOW + TTL, "the blessing was renewed");
    assert_eq!(
        renewals, 1,
        "a renewal was followed more than once, or never"
    );
}
