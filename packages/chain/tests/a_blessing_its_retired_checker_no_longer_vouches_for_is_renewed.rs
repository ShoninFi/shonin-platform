//! A key is blessed again unless the registry really counts it as blessed.
//! The key's record (`keyInfo`) can show most of a blessing left while the
//! registry no longer honours it: retiring the checker that gave a blessing
//! ends it until an allowed checker vouches for the key again. So a blessing
//! is skipped only when `isBlessedFor` says the key is blessed and its record
//! shows more than half the blessing left; otherwise the key could sit
//! unable to act until its record ran down to the renewal point.
//!
//! Needs the `args` feature, as every service has it.

#![cfg(feature = "args")]

mod stand_in;

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
const VAULT: Address = Address::repeat_byte(0x11);
const BUNDLE: B256 = B256::repeat_byte(0x42);
/// The checker that blessed the key, since retired.
const RETIRED: Address = Address::repeat_byte(0x77);

#[derive(Default)]
struct Registry {
    /// Whether the registry counts the key as blessed.
    blessed: bool,
    /// Every transaction handed to the node.
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
        "eth_sign" => Said::Result(json!(Bytes::from(vec![0x11u8; 65]))),
        "eth_sendRawTransaction" => {
            let raw = alloy::hex::decode(body["params"][0].as_str().unwrap()).unwrap();
            let hash = alloy::primitives::keccak256(&raw);
            registry.sent.push(hash);
            Said::Result(json!(hash))
        }
        // Mined at once, and it went through: the registry revives the key.
        "eth_getTransactionReceipt" => {
            let asked: B256 = body["params"][0].as_str().unwrap().parse().unwrap();
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
                // The key's record: this vault and bundle, nine tenths of
                // the blessing left, from a checker that may since have been
                // retired.
                IAttestationRegistry::keyInfoCall::SELECTOR => {
                    IAttestationRegistry::keyInfoCall::abi_encode_returns(&KeyInfo {
                        imageDigest: chain::attest::image_digest("executor"),
                        projectId: chain::attest::project_id(),
                        kind: ImageKind::Executor,
                        strategyHash: BUNDLE,
                        vaultChain: U256::from(31337u64),
                        vault: VAULT,
                        trustRoot: B256::ZERO,
                        verifier: RETIRED,
                        blessedUntil: NOW + TTL * 9 / 10,
                    })
                }
                IAttestationRegistry::isBlessedForCall::SELECTOR => {
                    IAttestationRegistry::isBlessedForCall::abi_encode_returns(&registry.blessed)
                }
                IAttestationRegistry::isVerifierAllowedCall::SELECTOR => {
                    IAttestationRegistry::isVerifierAllowedCall::abi_encode_returns(
                        &registry.blessed,
                    )
                }
                _ => return stand_in::unknown_method(),
            };
            Said::Result(json!(alloy::hex::encode_prefixed(out)))
        }
        _ => stand_in::unknown_method(),
    }
}

/// Bless the key for the vault against a registry that does or doesn't
/// count it as blessed, and say how many transactions were sent.
async fn bless_with(blessed: bool) -> usize {
    let registry = Arc::new(Mutex::new(Registry {
        blessed,
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
    let deployments = deployments();

    // What the executor's serve checks before it asks for a blessing.
    let counted = client
        .read(
            deployments.attestation,
            IAttestationRegistry::isBlessedForCall {
                signer: key.address,
                vaultChain: U256::from(31337u64),
                vault: VAULT,
                strategyHash: BUNDLE,
            },
        )
        .await
        .unwrap();
    assert_eq!(counted, blessed);

    let blessing = tokio::time::timeout(
        Duration::from_secs(20),
        chain::attest::bless_for_vault(
            &client,
            &deployments,
            "executor",
            key.address,
            chain::attest::Sender::Own(Box::new(key.unmetered())),
            VAULT,
            BUNDLE,
        ),
    )
    .await
    .expect("the blessing never finished");
    if let Err(error) = &blessing {
        panic!("the blessing failed: {error:#}");
    }

    registry.lock().unwrap().sent.len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blessing_its_retired_checker_no_longer_vouches_for_is_renewed() {
    assert_eq!(
        bless_with(false).await,
        1,
        "a key the registry no longer counts as blessed was not blessed again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_blessing_that_still_holds_with_most_of_it_left_is_not_sent_again() {
    assert_eq!(
        bless_with(true).await,
        0,
        "a key blessed with nine tenths of its blessing left was blessed again"
    );
}

/// Keeping the key blessed, against a registry that does or doesn't count
/// it as blessed: how many blessings were sent within a few seconds.
async fn kept_blessed_with(blessed: bool) -> usize {
    let registry = Arc::new(Mutex::new(Registry {
        blessed,
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
    let renewing = chain::attest::keep_blessed(
        client.clone(),
        deployments(),
        "executor",
        key.address,
        chain::attest::Sender::Own(Box::new(key.unmetered())),
        VAULT,
        BUNDLE,
        || {},
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    renewing.abort();
    registry.lock().unwrap().sent.len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kept_blessing_its_retired_checker_no_longer_vouches_for_is_renewed_at_once() {
    assert!(
        kept_blessed_with(false).await >= 1,
        "a key the registry no longer counts as blessed was left until its record ran down"
    );
    assert_eq!(
        kept_blessed_with(true).await,
        0,
        "a key blessed with nine tenths of its blessing left was renewed"
    );
}
