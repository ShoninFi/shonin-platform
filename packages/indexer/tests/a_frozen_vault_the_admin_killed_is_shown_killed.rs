//! A frozen vault the admin then kills is shown as killed. After a week
//! anyone can freeze a vault, and the admin can then kill it. `kill` on a
//! vault already halted emits no `Halted` (the halt is kept as it was), only
//! `StateChanged(Killed)`; that is read as the kill, so it is in the feed,
//! and once the vault closes it stays shown as stopped, not as an ordinary
//! close.
//!
//! Here: the vault's log holds `Halted("frozen")`, `StateChanged(Killed)`
//! and `StateChanged(Closed)`, and it reads `Closed`.

mod stand_in;

use std::sync::Arc;

use alloy::primitives::{B256, U256};
use chain::bindings::vault::{IVault, VaultState};
use serde_json::json;
use shared::model::activity::EventKind;
use shared::model::registry::FeedKind;
use shared::model::strategy::Standing;
use stand_in::*;

fn reason(text: &str) -> B256 {
    let mut word = [0u8; 32];
    word[..text.len()].copy_from_slice(text.as_bytes());
    B256::from(word)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frozen_vault_the_admin_killed_is_shown_killed() {
    let indexer = an_indexer(
        Arc::new(|request| match request["method"].as_str() {
            Some("eth_getLogs") if asks_the_vault(request) => Some(Ok(json!([
                logged(
                    IVault::Halted {
                        reason: reason("frozen"),
                        haltedAt: 1_790_000_003,
                    },
                    3,
                    0
                ),
                logged(
                    IVault::StateChanged {
                        state: VaultState::Killed
                    },
                    4,
                    0
                ),
                logged(
                    IVault::StateChanged {
                        state: VaultState::Closed
                    },
                    5,
                    0
                ),
            ]))),
            Some("eth_call") if input(request) == selector::<IVault::stateCall>() => {
                Some(Ok(json!(format!("0x{:064x}", U256::from(6u8)))))
            }
            _ => None,
        }),
        "frozen-then-killed",
    )
    .await;
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await;
    let strategy = &snapshot.strategies[0];
    assert!(
        strategy.kill.is_some() && strategy.standing == Standing::Killed,
        "a vault the admin killed after it froze reads as {:?}, with kill {:?}",
        strategy.standing,
        strategy.kill.as_ref().map(|kill| kill.reference.clone())
    );
    assert!(
        snapshot
            .feed
            .iter()
            .any(|event| event.kind == FeedKind::Kill),
        "the kill of a frozen vault is not in the feed"
    );
}

/// A kill of a vault still running halts it in the same transaction: told of
/// once, by its `Halted`, not again by its `StateChanged(Killed)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_killed_while_running_is_told_of_once() {
    let indexer = an_indexer(
        Arc::new(|request| match request["method"].as_str() {
            Some("eth_getLogs") if asks_the_vault(request) => Some(Ok(json!([
                logged(
                    IVault::Halted {
                        reason: reason("killed"),
                        haltedAt: 1_790_000_003,
                    },
                    3,
                    0
                ),
                logged(
                    IVault::StateChanged {
                        state: VaultState::Killed
                    },
                    3,
                    0
                ),
            ]))),
            Some("eth_call") if input(request) == selector::<IVault::stateCall>() => {
                Some(Ok(json!(format!("0x{:064x}", U256::from(5u8)))))
            }
            _ => None,
        }),
        "killed-while-running",
    )
    .await;
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await;
    let kills = snapshot.strategies[0]
        .events
        .iter()
        .filter(|event| event.kind == EventKind::Killed)
        .count();
    assert_eq!(
        kills, 1,
        "a kill of a running vault was told of {kills} times"
    );
}
