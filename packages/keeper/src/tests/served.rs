//! Rounds read from an observation make the decisions rounds reading the
//! chain itself make, and read no view of the chain to make them.
//!
//! Each test runs the same scripted vault twice, on two stand-in nodes: once
//! with every round reading the chain itself (the active path, as the keeper
//! always has), and once with every round after the first read from an
//! observation of what the keeper read before, answered by the node at its
//! head as a batched read would. Both must send the same transactions.

use std::ops::Deref;
use std::sync::Arc;
use std::time::Instant;

use alloy::network::TransactionBuilder;
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use chain::follow::{self, Item, Path};

use super::*;
use crate::observed::{self, Snapshot};
use crate::stand_in::{self, Shared};

/// An observation of every read the keeper has made on the chain, answered
/// by the node at its head, and a round's reads answered from it.
async fn looked(client: &Observed, learned: &observed::Shared, node: &Shared) -> Observed {
    let plan = learned.lock().unwrap().plan(Instant::now());
    let mut items = Vec::new();
    for read in &plan.reads {
        let request = TransactionRequest::default()
            .with_to(read.target)
            .with_input(read.data.clone());
        items.push(match client.provider().call(request).await {
            Ok(answer) => Item::Returned(answer),
            Err(_) => Item::Reverted(Bytes::new()),
        });
    }
    let (number, timestamp) = {
        let node = node.lock().unwrap();
        (node.head, node.timestamp)
    };
    let observed = follow::Observed {
        endpoint: 0,
        path: Path::ViewReads,
        number,
        timestamp,
        // The stand-in's blocks carry no hashes.
        parent_hash: B256::ZERO,
        hash: Some(B256::repeat_byte(2)),
        hashes: Default::default(),
        zero: Vec::new(),
        balances: vec![U256::ZERO; plan.balances.len()],
        items,
        wall_ms: 0,
        gas_at_entry: None,
        requests: 1,
    };
    let snapshot = Arc::new(Snapshot::new(&plan, &observed));
    learned.lock().unwrap().observed(&snapshot);
    node.lock().unwrap().asked.clear();
    Observed::served(client.deref().clone(), learned.clone(), snapshot, None)
}

/// The views a round read of the chain itself since `looked`.
fn views_read(node: &Shared) -> Vec<(Address, [u8; 4])> {
    node.lock()
        .unwrap()
        .asked
        .iter()
        .filter_map(|(_, call)| *call)
        .filter(|(_, selector)| observed::kind_of(selector).is_some())
        .collect()
}

/// `rounds` rounds of the vault on `node`, every one reading the chain
/// itself (`served` false) or every one after the first read from an
/// observation; the clock moves `step` seconds after each.
async fn rounds(node: &Shared, client: &Observed, rounds: usize, step: u64, served: bool) {
    let learned = observed::Shared::default();
    let mut memory = Memory::default();
    for round in 0..rounds {
        let reads = if served && round > 0 {
            looked(client, &learned, node).await
        } else {
            Observed::live(client.deref().clone(), learned.clone())
        };
        let _ = keep_one(
            &reads,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await;
        if served && round > 0 && reads.sends() == 0 {
            assert_eq!(
                views_read(node),
                Vec::new(),
                "a round read from an observation, sending nothing, read views of the chain"
            );
        }
        node.lock().unwrap().timestamp += step;
    }
}

/// The same scripted vault, kept both ways: what each sent.
async fn both_ways(
    script: impl Fn(&Shared),
    start: u64,
    block_time: Option<u64>,
    count: usize,
    step: u64,
) -> (Vec<String>, Vec<String>) {
    let (live, served) = both_ways_when(script, start, block_time, count, step).await;
    assert_eq!(live, served, "sent, with the chain time each was sent at");
    let names = |sent: Vec<(String, u64)>| -> Vec<String> {
        sent.into_iter()
            .map(|(call, _)| call.chars().take(53).collect())
            .collect()
    };
    (names(live), names(served))
}

/// [`both_ways`], with each send's whole calldata and the chain time it was
/// sent at.
async fn both_ways_when(
    script: impl Fn(&Shared),
    start: u64,
    block_time: Option<u64>,
    count: usize,
    step: u64,
) -> (Vec<(String, u64)>, Vec<(String, u64)>) {
    let mut sent = Vec::new();
    for served in [false, true] {
        let node = stand_in::node(start);
        script(&node);
        let client = match block_time {
            Some(seconds) => connect_with_blocks_of(&node, seconds).await,
            None => connect(&node).await,
        };
        rounds(&node, &client, count, step, served).await;
        sent.push(stand_in::everything_sent_when(&node));
    }
    let served = sent.pop().unwrap();
    let live = sent.pop().unwrap();
    (live, served)
}

#[tokio::test]
async fn a_halted_vault_read_from_observations_is_valued_stopped_and_closed_as_before() {
    let start = SLOT_START + 1;
    let (live, served) = both_ways(
        |node| halted_and_empty(node, start, start - 2 * 600),
        start,
        Some(2),
        30,
        2,
    )
    .await;
    assert!(!live.is_empty());
    assert_eq!(live, served);
}

#[tokio::test]
async fn a_waiting_claim_read_from_observations_is_tried_in_the_same_slots_as_before() {
    let start = BETWEEN_SLOTS;
    let (live, served) = both_ways(
        |node| {
            an_open_vault(node, a_tank(start, 0, start));
            a_waiting_claim(node, start, ClaimState::Waiting);
            answers(node, STATION, IStation::outsideBandCall {}, false);
            answers(
                node,
                STATION,
                IStation::stageCall {},
                chain::bindings::station::Stage {
                    fraction: U256::ZERO,
                    owed: U256::ZERO,
                    estimate: U256::ZERO,
                    fetched: U256::ZERO,
                    cost: U256::ZERO,
                    part: U256::ZERO,
                    arrivedAt: U256::ZERO,
                    stagedAt: 0,
                    stagedBlock: 0,
                    sweptAt: 0,
                    sweptBlock: 0,
                    window: 0,
                    delay: 0,
                    forVault: false,
                },
            );
            takes(
                node,
                VAULT,
                IVault::settleClaimCall {
                    claimId: U256::ZERO,
                },
            );
        },
        start,
        None,
        40,
        3,
    )
    .await;
    let try_sent = format!(
        "{VAULT} {}",
        alloy::hex::encode_prefixed(IVault::settleClaimCall::SELECTOR)
    );
    assert!(
        live.contains(&try_sent),
        "the claim was never tried: {live:?}"
    );
    assert_eq!(live, served);
}

#[tokio::test]
async fn an_unopened_vault_read_from_observations_is_cancelled_when_its_window_ends() {
    let start = BETWEEN_SLOTS;
    let (live, served) = both_ways(
        |node| {
            answers(node, VAULT, IVault::stateCall {}, VaultState::Activating);
            answers(node, VAULT, IVault::timersCall {}, timers());
            answers(
                node,
                VAULT,
                IVault::createdAtCall {},
                start - timers().activationWindow + 10,
            );
            takes(node, VAULT, IVault::cancelCall {});
        },
        start,
        None,
        8,
        3,
    )
    .await;
    let cancel = format!(
        "{VAULT} {}",
        alloy::hex::encode_prefixed(IVault::cancelCall::SELECTOR)
    );
    assert!(live.contains(&cancel), "{live:?}");
    assert_eq!(live, served);
}

/// The registry as the revoke gate reads it, for the station's one driver
/// and `key`.
fn a_registry(node: &Shared, key: Address, blessed: bool, kind: ImageKindAlias) {
    // The driver's exit path, which the delayed venue's step reads: not
    // delayed.
    answers(
        node,
        Address::repeat_byte(0x63),
        chain::bindings::driver::IDriver::exitPathCall {},
        chain::bindings::driver::ExitPath::Contract,
    );
    use chain::bindings::attestation::{IAttestationRegistry, KeyInfo};
    use observed::IMadeBy;
    let made_by = Address::repeat_byte(0x61);
    let registry = Address::repeat_byte(0x62);
    let local = name("local");
    let implementation = Address::repeat_byte(0x63);
    answers(node, STATION, IStation::stoppedCall {}, false);
    answers(
        node,
        STATION,
        IStation::reclaimedCall { driver: local },
        false,
    );
    answers(node, STATION, IMadeBy::factoryCall {}, made_by);
    answers(node, made_by, IMadeBy::attestationCall {}, registry);
    answers(
        node,
        STATION,
        IStation::pinnedDriverCall { driver: local },
        implementation,
    );
    answers(
        node,
        registry,
        IAttestationRegistry::isDriverRevokedCall {
            name: local,
            implementation,
        },
        false,
    );
    answers(
        node,
        registry,
        IAttestationRegistry::isBlessedForCall {
            signer: key,
            vaultChain: U256::from(31337u64),
            vault: VAULT,
            strategyHash: B256::ZERO,
        },
        blessed,
    );
    answers(
        node,
        registry,
        IAttestationRegistry::keyInfoCall { signer: key },
        KeyInfo {
            imageDigest: B256::ZERO,
            projectId: B256::ZERO,
            kind,
            strategyHash: B256::ZERO,
            vaultChain: U256::from(31337u64),
            vault: VAULT,
            trustRoot: B256::ZERO,
            verifier: Address::ZERO,
            blessedUntil: 0,
        },
    );
}

type ImageKindAlias = chain::bindings::attestation::ImageKind;

/// Transactions the node was asked to take, taken or refused.
fn sends_asked(node: &Shared) -> usize {
    node.lock()
        .unwrap()
        .asked
        .iter()
        .filter(|(method, _)| method == "eth_sendTransaction" || method == "eth_estimateGas")
        .count()
}

#[tokio::test]
async fn read_from_an_observation_a_revoke_the_station_would_refuse_is_never_tried() {
    let key = Address::repeat_byte(0x77);
    let node = stand_in::node(BETWEEN_SLOTS);
    an_open_vault_with_a_key(&node, key);
    a_registry(&node, key, true, ImageKindAlias::Executor);
    // The station refuses: the key's blessing stands.
    stand_in::refuses_as(
        &node,
        STATION,
        IStation::revokeKeyCall {
            driver: B256::ZERO,
            key,
        },
        "StillBlessed()",
    );
    let client = connect(&node).await;
    let learned = observed::Shared::default();
    let mut memory = Memory::default();
    let live = Observed::live(client.deref().clone(), learned.clone());
    let _ = keep_one(
        &live,
        KEEPER,
        &deployments(),
        "http://127.0.0.1:1",
        VAULT,
        &mut memory,
    )
    .await;
    let asked_live = sends_asked(&node);
    assert!(
        asked_live >= 1,
        "the active path tries the revoke as before"
    );
    // The first look holds what the active path read; the gate's own views
    // are read of the chain that once, and are in every look after.
    for _ in 0..2 {
        let served = looked(&client, &learned, &node).await;
        let _ = keep_one(
            &served,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await;
        assert_eq!(sends_asked(&node), 0, "a refused revoke was tried");
    }
    assert_eq!(views_read(&node), Vec::new());
}

#[tokio::test]
async fn read_from_an_observation_a_revoke_the_station_would_take_is_sent() {
    for (blessed, kind) in [
        (false, ImageKindAlias::Executor),
        (true, ImageKindAlias::Builder),
    ] {
        let key = Address::repeat_byte(0x77);
        let node = stand_in::node(BETWEEN_SLOTS);
        an_open_vault_with_a_key(&node, key);
        a_registry(&node, key, blessed, kind);
        stand_in::takes(
            &node,
            STATION,
            IStation::revokeKeyCall {
                driver: B256::ZERO,
                key,
            },
        );
        let client = connect(&node).await;
        let learned = observed::Shared::default();
        let mut memory = Memory::default();
        let live = Observed::live(client.deref().clone(), learned.clone());
        let _ = keep_one(
            &live,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await;
        let served = looked(&client, &learned, &node).await;
        let _ = keep_one(
            &served,
            KEEPER,
            &deployments(),
            "http://127.0.0.1:1",
            VAULT,
            &mut memory,
        )
        .await;
        assert_eq!(
            revokes_of(&node, key),
            2,
            "blessed {blessed}, kind {}: the revoke went once each way",
            kind as u8
        );
    }
}

#[tokio::test]
async fn after_a_send_every_read_of_the_round_is_of_the_chain_itself() {
    let start = BETWEEN_SLOTS;
    let node = stand_in::node(start);
    answers(&node, VAULT, IVault::stateCall {}, VaultState::Activating);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(
        &node,
        VAULT,
        IVault::createdAtCall {},
        start - timers().activationWindow - 1,
    );
    takes(&node, VAULT, IVault::cancelCall {});
    let client = connect(&node).await;
    let learned = observed::Shared::default();
    let live = Observed::live(client.deref().clone(), learned.clone());
    live.read(VAULT, IVault::stateCall {}).await.unwrap();
    let served = looked(&client, &learned, &node).await;
    assert_eq!(
        served.read(VAULT, IVault::stateCall {}).await.unwrap() as u8,
        VaultState::Activating as u8
    );
    assert!(views_read(&node).is_empty());
    served
        .send(KEEPER, VAULT, IVault::cancelCall {})
        .await
        .unwrap();
    answers(&node, VAULT, IVault::stateCall {}, VaultState::Cancelled);
    assert_eq!(
        served.read(VAULT, IVault::stateCall {}).await.unwrap() as u8,
        VaultState::Cancelled as u8,
        "a read after a send was answered from the observation taken before it"
    );
    assert_eq!(served.sends(), 1);
}

/// An open vault at `vault`, never priced, so it wants a valuation in any
/// slot, with `claims` waiting claims to try, and its own station.
fn a_vault_wanting_a_valuation(
    node: &Shared,
    vault: Address,
    station: Address,
    now: u64,
    claims: u64,
) {
    answers(node, vault, IVault::stateCall {}, VaultState::Open);
    answers(node, vault, IVault::timersCall {}, timers());
    answers(node, vault, IVault::tankCall {}, a_tank(now, 0, now));
    answers(node, vault, IStationVault::haltedAtCall {}, 0);
    answers(node, vault, IVault::homeStationCall {}, station);
    answers(node, station, IStation::specCall {}, a_spec(vec![]));
    answers(node, vault, IVault::valuationCountCall {}, U256::ZERO);
    answers(node, vault, IStationVault::unsettledCall {}, false);
    answers(node, station, IStation::outsideBandCall {}, false);
    answers(
        node,
        station,
        IStation::stageCall {},
        chain::bindings::station::Stage {
            fraction: U256::ZERO,
            owed: U256::ZERO,
            estimate: U256::ZERO,
            fetched: U256::ZERO,
            cost: U256::ZERO,
            part: U256::ZERO,
            arrivedAt: U256::ZERO,
            stagedAt: 0,
            stagedBlock: 0,
            sweptAt: 0,
            sweptBlock: 0,
            window: 0,
            delay: 0,
            forVault: false,
        },
    );
    answers(node, vault, IVault::claimCountCall {}, U256::from(claims));
    answers(
        node,
        vault,
        IVault::claimAtCall {
            claimId: U256::from(1u64),
        },
        Claim {
            owner: Address::repeat_byte(0x44),
            shares: U256::from(WAD),
            bookedAt: now - 100,
            lastAttemptAt: 0,
            state: ClaimState::Waiting,
            paid: U256::ZERO,
        },
    );
}

/// Two vaults wanting a valuation in the same slot, the first in the
/// factory's list with two claims to try as well, on a chain of two-second
/// blocks whose head is a second before the slot: one round, read from an
/// observation or of the chain itself. The valuations each would send, and
/// the moments they were sent at.
async fn two_vaults_one_round(served: bool) -> Vec<(Address, u64)> {
    let first = Address::repeat_byte(0x11);
    let second = Address::repeat_byte(0x12);
    let slot = SLOT_START + 10;
    let start = slot - 1;
    let node = stand_in::node(start);
    a_vault_wanting_a_valuation(&node, first, Address::repeat_byte(0x21), start, 2);
    a_vault_wanting_a_valuation(&node, second, Address::repeat_byte(0x22), start, 0);
    let factory = deployments().factory;
    answers(
        &node,
        factory,
        IVaultFactory::vaultsCall {},
        vec![first, second],
    );
    answers(
        &node,
        factory,
        IReplacesFactory::previousCall {},
        Address::ZERO,
    );
    stand_in::keeps_the_window(&node, 10, 5);
    let client = connect_with_blocks_of(&node, 2).await;
    let learned = observed::Shared::default();
    let mut memory = HashMap::new();
    let mut factories = Factories::default();
    // A first look learns what the round reads, at a moment nothing is due.
    node.lock().unwrap().timestamp = start - 5;
    let live = Observed::live(client.deref().clone(), learned.clone());
    let _ = tick(
        Chain::Base,
        &live,
        &deployments(),
        KEEPER,
        "http://127.0.0.1:1",
        &mut memory,
        &mut factories,
    )
    .await;
    {
        // Some blocks later.
        let mut node = node.lock().unwrap();
        node.timestamp = start;
        node.head += 3;
    }
    let before = stand_in::everything_sent_when(&node).len();
    let reads = if served {
        looked(&client, &learned, &node).await
    } else {
        Observed::live(client.deref().clone(), learned.clone())
    };
    let _ = tick(
        Chain::Base,
        &reads,
        &deployments(),
        KEEPER,
        "http://127.0.0.1:1",
        &mut memory,
        &mut factories,
    )
    .await;
    let valued = alloy::hex::encode_prefixed(IVault::recordValuationCall::SELECTOR);
    stand_in::everything_sent_when(&node)[before..]
        .iter()
        .filter(|(call, _)| call.ends_with(&valued))
        .map(|(call, at)| (call[..42].parse().unwrap(), *at))
        .collect()
}

#[tokio::test]
async fn read_from_an_observation_every_vault_s_valuation_goes_before_any_vault_s_other_sends() {
    let landing_in_window = |at: u64| (at + 2) % 10 <= 5;
    let served = two_vaults_one_round(true).await;
    assert_eq!(served.len(), 2, "both vaults valued: {served:?}");
    assert!(
        served.iter().all(|(_, at)| landing_in_window(*at)),
        "{served:?}"
    );
    // Read of the chain itself, vault by vault, as before: the first vault's
    // other sends take the blocks the second's valuation needed.
    let live = two_vaults_one_round(false).await;
    assert_eq!(live.len(), 1, "{live:?}");
}

#[tokio::test]
async fn a_send_refused_at_its_estimate_leaves_the_round_reading_its_observation() {
    let start = BETWEEN_SLOTS;
    let node = stand_in::node(start);
    answers(&node, VAULT, IVault::stateCall {}, VaultState::Activating);
    stand_in::refuses(&node, VAULT, IVault::cancelCall {});
    let client = connect(&node).await;
    let learned = observed::Shared::default();
    let live = Observed::live(client.deref().clone(), learned.clone());
    live.read(VAULT, IVault::stateCall {}).await.unwrap();
    let served = looked(&client, &learned, &node).await;
    assert!(
        served
            .send(KEEPER, VAULT, IVault::cancelCall {})
            .await
            .is_err()
    );
    served.read(VAULT, IVault::stateCall {}).await.unwrap();
    assert_eq!(views_read(&node), Vec::new());
    assert_eq!(served.sends(), 0, "a refused send was counted as gone out");
}

#[tokio::test]
async fn a_view_an_observation_answered_with_a_revert_is_read_again_of_the_chain_itself() {
    let node = stand_in::node(BETWEEN_SLOTS);
    answers(&node, VAULT, IVault::stateCall {}, VaultState::Activating);
    let client = connect(&node).await;
    let learned = observed::Shared::default();
    Observed::live(client.deref().clone(), learned.clone())
        .read(VAULT, IVault::stateCall {})
        .await
        .unwrap();
    let plan = learned.lock().unwrap().plan(Instant::now());
    assert_eq!(plan.reads.len(), 1);
    // A read the batch gave too little gas for inside (a call it makes runs
    // out) comes back as a revert with nothing to say.
    let observed = follow::Observed {
        endpoint: 0,
        path: Path::ViewReads,
        number: 16,
        timestamp: BETWEEN_SLOTS,
        parent_hash: B256::repeat_byte(1),
        hash: Some(B256::repeat_byte(2)),
        hashes: Default::default(),
        zero: Vec::new(),
        balances: Vec::new(),
        items: vec![Item::Reverted(Bytes::new())],
        wall_ms: 0,
        gas_at_entry: None,
        requests: 1,
    };
    let snapshot = Arc::new(Snapshot::new(&plan, &observed));
    node.lock().unwrap().asked.clear();
    let served = Observed::served(client.deref().clone(), learned, snapshot, None);
    assert_eq!(
        served.read(VAULT, IVault::stateCall {}).await.unwrap() as u8,
        VaultState::Activating as u8
    );
    assert_eq!(
        views_read(&node).len(),
        1,
        "the revert was taken as the answer"
    );
}

#[tokio::test]
async fn a_station_once_read_stays_watched_for_logs_after_a_round_that_sent() {
    let node = stand_in::node(BETWEEN_SLOTS);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    let client = connect(&node).await;
    let learned = observed::Shared::default();
    Observed::live(client.deref().clone(), learned.clone())
        .read(VAULT, IVault::homeStationCall {})
        .await
        .unwrap();
    let mut stations = HashMap::new();
    let looked_at = looked(&client, &learned, &node).await;
    assert_eq!(
        watched(&looked_at, &[VAULT], &mut stations),
        vec![VAULT, STATION]
    );
    // A round that read the chain itself holds nothing to peek at.
    let live = Observed::live(client.deref().clone(), observed::Shared::default());
    assert_eq!(
        watched(&live, &[VAULT], &mut stations),
        vec![VAULT, STATION]
    );
}

/// An open vault wanting a valuation, its window open for a send made now,
/// on an endpoint whose reads by number reach a node `behind` blocks behind
/// its head.
async fn valued_behind_a_lagging_node(behind: u64, learned: observed::Shared) -> (usize, bool) {
    let slot = SLOT_START + 10;
    let start = slot - 1;
    let node = stand_in::node(start);
    a_vault_wanting_a_valuation(&node, VAULT, STATION, start, 0);
    stand_in::keeps_the_window(&node, 10, 5);
    let client = connect_with_blocks_of(&node, 2).await;
    node.lock().unwrap().numbers_behind = behind;
    let head = node.lock().unwrap().head;
    // Read the old way, one read for the head and another for its block,
    // the second reaches the node behind.
    let old_way_fails = client.block_timestamp_at(head).await.is_err();
    let reads = Observed::live(client.deref().clone(), learned);
    let mut memory = Memory::default();
    let _ = keep_one(
        &reads,
        KEEPER,
        &deployments(),
        "http://127.0.0.1:1",
        VAULT,
        &mut memory,
    )
    .await;
    (
        sent::<IVault::recordValuationCall>(&node, VAULT),
        old_way_fails,
    )
}

#[tokio::test]
async fn the_block_interval_is_measured_when_the_next_read_reaches_a_node_a_block_behind() {
    let (valued, old_way_fails) =
        valued_behind_a_lagging_node(1, observed::Shared::default()).await;
    assert!(
        old_way_fails,
        "setup: the head block read by number must be missing"
    );
    assert_eq!(valued, 1, "the vault's valuation was skipped");
}

#[tokio::test]
async fn a_block_interval_that_cannot_be_measured_is_the_last_one_measured_and_skips_nothing() {
    let learned = observed::Shared::default();
    learned.lock().unwrap().block_interval = Some(2);
    // The parent is out of reach too.
    let (valued, _) = valued_behind_a_lagging_node(2, learned).await;
    assert_eq!(valued, 1, "the vault's valuation was skipped");
}

#[tokio::test]
async fn after_a_send_logs_are_still_read_only_where_they_were_checked() {
    use alloy::sol_types::SolEvent;
    let start = BETWEEN_SLOTS;
    let node = stand_in::node(start);
    answers(&node, VAULT, IVault::stateCall {}, VaultState::Activating);
    takes(&node, VAULT, IVault::cancelCall {});
    let client = connect(&node).await;
    let learned = observed::Shared::default();
    Observed::live(client.deref().clone(), learned.clone())
        .read(VAULT, IVault::stateCall {})
        .await
        .unwrap();
    let served = looked(&client, &learned, &node).await;
    let checked = observed::Logs {
        through: 10,
        from: HashMap::from([(VAULT, 1)]),
        topics: crate::logs::topics(),
        logs: Vec::new(),
    };
    let served = Observed::served(
        client.deref().clone(),
        learned,
        served.snapshot().unwrap().clone(),
        Some(Arc::new(checked)),
    );
    served
        .send(KEEPER, VAULT, IVault::cancelCall {})
        .await
        .unwrap();
    assert_eq!(served.log_head().await.unwrap(), 10);
    let filter = alloy::rpc::types::Filter::new()
        .address(VAULT)
        .event_signature(IVault::ProRataTaken::SIGNATURE_HASH)
        .from_block(1)
        .to_block(10);
    assert!(served.logs(&filter).await.unwrap().is_empty());
    assert!(
        !node
            .lock()
            .unwrap()
            .asked
            .iter()
            .any(|(method, _)| method == "eth_getLogs" || method == "eth_blockNumber"),
        "logs were read of the chain itself, unchecked, after a send"
    );
}
