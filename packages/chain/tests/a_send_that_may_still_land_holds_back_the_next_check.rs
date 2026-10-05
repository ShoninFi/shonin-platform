//! A send gives the key's turn up at its deadline (60 s) while its
//! transaction may still be in the pool and watched for. A check made in the
//! turn after that reads the chain as if that transaction did not exist, so a
//! client noting its sends in an `Unanswered` doesn't check, or send, while a
//! send noted there may still land: a second blessing or approval would land
//! as well and both be paid for.
//!
//! The sequence that gets there (two serves of one vault, sharing
//! `blessings` as the executor does):
//! 1. Serve 1's keeper finds a renewal due and starts its send. Filling the
//!    transaction in is slow (a slow node), so nothing is noted yet.
//! 2. Serve 1 ends; its keeper is aborted. The broadcast task carries on.
//! 3. Serve 2 starts, sees `blessings.may_still_land(key) == false` (the
//!    first send is still being filled in, so it isn't noted) and calls
//!    `bless_for_vault`, whose `send_if` waits for the key's turn.
//! 4. Serve 1's broadcast goes out, is noted, and doesn't land within the
//!    60 s deadline: the turn is given up.
//! 5. Serve 2 is held back while the first may still land, rather than
//!    reading the blessing unchanged and sending a second.
//!
//! That takes a little over a minute: the turn is only given up at the real
//! 60 s deadline. The same hold, on a send noted from the start, is quick.

#![cfg(feature = "args")]

mod stand_in;

use std::collections::HashSet;
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

#[derive(Default)]
struct Registry {
    blessed_until: u64,
    sent: Vec<B256>,
    mined: HashSet<B256>,
    /// How many gas estimates have been asked for.
    estimates: usize,
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
    // The first gas estimate is slow: a node under load. Held outside the
    // lock so other requests are answered meanwhile.
    if method == "eth_estimateGas" {
        let first = {
            let mut registry = registry.lock().unwrap();
            registry.estimates += 1;
            registry.estimates == 1
        };
        if first {
            std::thread::sleep(Duration::from_secs(3));
        }
        return Said::Result(json!("0x30000"));
    }
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
        // The pending count, as a node gives it: every transaction in the
        // pool takes a nonce.
        "eth_getTransactionCount" => Said::Result(json!(format!("0x{:x}", registry.sent.len()))),
        "eth_sign" => Said::Result(json!(Bytes::from(vec![0x11u8; 65]))),
        "eth_sendRawTransaction" => {
            let raw = alloy::hex::decode(body["params"][0].as_str().unwrap()).unwrap();
            let hash = alloy::primitives::keccak256(&raw);
            registry.sent.push(hash);
            Said::Result(json!(hash))
        }
        "eth_getTransactionReceipt" => {
            let asked: B256 = body["params"][0].as_str().unwrap().parse().unwrap();
            if !registry.mined.contains(&asked) {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blessing_that_gave_the_turn_up_and_may_still_land_holds_back_a_second() {
    // A quarter of the blessing left: a renewal is due.
    let registry = Arc::new(Mutex::new(Registry {
        blessed_until: NOW + TTL / 4,
        ..Registry::default()
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
    let platform = key.unmetered();
    let vault = Address::repeat_byte(0x11);
    let bundle = B256::repeat_byte(0x42);
    // Kept with the vault across serves, as the executor keeps it.
    let blessings = chain::Unanswered::default();

    // 1. Serve 1's keeper starts a renewal; its fill is slow.
    let renewing = chain::attest::keep_blessed_sharing(
        client.clone(),
        deployments(),
        "executor",
        key.address,
        chain::attest::Sender::Own(Box::new(platform.clone())),
        vault,
        bundle,
        blessings.clone(),
        || {},
    );
    for _ in 0..400 {
        if registry.lock().unwrap().estimates > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(registry.lock().unwrap().estimates, 1, "the renewal began");

    // 2. Serve 1 ends; its keeper is stopped mid-send.
    renewing.abort();

    // 3. Serve 2 starts, as `serve` does: nothing is noted yet, so it blesses.
    assert!(
        !blessings.may_still_land(key.address),
        "precondition: the first send is still being filled in, so it isn't noted"
    );
    let second = {
        let client = client.clone();
        let sender = chain::attest::Sender::Own(Box::new(platform.noting(&blessings)));
        tokio::spawn(async move {
            chain::attest::bless_for_vault(
                &client,
                &deployments(),
                "executor",
                key.address,
                sender,
                vault,
                bundle,
            )
            .await
        })
    };

    // 4. The first renewal goes out and sits in the pool, unmined.
    for _ in 0..800 {
        if !registry.lock().unwrap().sent.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        registry.lock().unwrap().sent.len(),
        1,
        "the first renewal went out"
    );
    assert!(
        blessings.may_still_land(key.address),
        "the first renewal is noted as one that may still land"
    );

    // 5. Past the first send's 60 s deadline its turn is given up; the second
    //    caller's in-turn check runs while the first may still land.
    for _ in 0..150 {
        if registry.lock().unwrap().sent.len() > 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    second.abort();

    let sent = registry.lock().unwrap().sent.len();
    assert!(
        blessings.may_still_land(key.address),
        "the first renewal was still noted as one that may land"
    );
    assert_eq!(
        sent, 1,
        "a second blessing was sent by a check made in the key's turn while the first, \
         noted as one that may still land, was still in the pool: {sent} sent"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_noted_as_one_that_may_still_land_holds_back_the_next_check() {
    let registry = Arc::new(Mutex::new(Registry::default()));
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
    // An approval of this key's, sent by a serve since stopped, unanswered.
    let approvals = chain::Unanswered::default();
    approvals.failed(key.address, &anyhow::Error::new(chain::NeverLanded));
    assert!(approvals.may_still_land(key.address));

    let checks = Arc::new(Mutex::new(0usize));
    let sending = {
        let key = key.unmetered().noting(&approvals);
        let checks = checks.clone();
        tokio::spawn(async move {
            key.send_if(
                async move {
                    *checks.lock().unwrap() += 1;
                    Ok(true)
                },
                Address::repeat_byte(0x22),
                IAttestationRegistry::blessingTtlCall {},
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        (*checks.lock().unwrap(), registry.lock().unwrap().sent.len()),
        (0, 0),
        "a check was made, and acted on, while a send noted as one that may still land could"
    );
    // The key's turn is not held meanwhile: its other sends go ahead.
    let other = key.clone();
    let other = tokio::spawn(async move {
        other
            .send(
                Address::repeat_byte(0x33),
                IAttestationRegistry::blessingTtlCall {},
            )
            .await
    });
    for _ in 0..800 {
        if !registry.lock().unwrap().sent.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        registry.lock().unwrap().sent.len(),
        1,
        "the key's turn was held"
    );
    other.abort();

    // Once the earlier send is seen done, the check is made and its send goes.
    approvals.settled();
    let first = registry.lock().unwrap().sent[0];
    registry.lock().unwrap().mined.insert(first);
    for _ in 0..800 {
        if registry.lock().unwrap().sent.len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(*checks.lock().unwrap(), 1);
    assert_eq!(registry.lock().unwrap().sent.len(), 2);
    sending.abort();
}
