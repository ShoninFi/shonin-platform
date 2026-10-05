//! A renewal whose keeper is stopped halfway through its send (the serve
//! that holds the keeper ends) may still land: the broadcast carries on
//! without it. The send notes itself as one that may still land the moment
//! it is broadcast, in the `Unanswered` shared with the next serve, so the
//! next serve doesn't bless the key again while the first is on its way:
//! both would land, each paid for.
//!
//! Needs the `args` feature, as every service has it.

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
    /// Every transaction handed to the node.
    sent: Vec<B256>,
    /// Those mined so far.
    mined: HashSet<B256>,
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
        // Taken into the pool; mined only when the test says.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_renewal_whose_keeper_is_stopped_mid_send_still_holds_the_next_back() {
    // A quarter of the blessing is left: a renewal is due.
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
    // Kept with the vault across serves, as the executor does.
    let blessings = chain::Unanswered::default();

    // The first serve's keeper renews; the renewal is broadcast and sits in
    // the pool.
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
    for _ in 0..200 {
        if !registry.lock().unwrap().sent.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        registry.lock().unwrap().sent.len(),
        1,
        "the renewal went out"
    );

    // The serve ends (a read failed, say) and its keeper is stopped
    // mid-send.
    renewing.abort();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        blessings.may_still_land(key.address),
        "a renewal broadcast by a keeper since stopped is not noted as one that may still land"
    );

    // The serve started again: the blessing reads unchanged (the renewal
    // hasn't landed), so it blesses unless a blessing may still land.
    let second = if !blessings.may_still_land(key.address) {
        let client = client.clone();
        let sender = chain::attest::Sender::Own(Box::new(platform.clone()));
        Some(tokio::spawn(async move {
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
        }))
    } else {
        None
    };
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The first renewal lands.
    {
        let mut registry = registry.lock().unwrap();
        let first = registry.sent[0];
        registry.mined.insert(first);
        registry.blessed_until = NOW + TTL;
    }
    for _ in 0..120 {
        if registry.lock().unwrap().sent.len() > 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    if let Some(second) = second {
        second.abort();
    }

    let sent = registry.lock().unwrap().sent.len();
    assert_eq!(
        sent, 1,
        "the next serve blessed again while a renewal its stopped keeper had broadcast \
         was still on its way: {sent} blessings sent"
    );
}
