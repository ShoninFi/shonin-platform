//! The keeper keeps the vaults of every factory the current one replaces.
//! An earlier factory's vaults were configured by that factory (its timers
//! are its config, not the keeper's), and its list may be unreadable for
//! good (a node's `eth_call` gas cap on an unpaginated list). Neither may
//! keep the current factory's vaults from being kept when the keeper starts:
//! such a vault answers for itself in its own rounds, and the chain's other
//! vaults are kept.
mod process;

use alloy::primitives::Address;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::vault::{IVault, VaultState};
use process::{
    FACTORY, IReplacesFactory, VAULT, answers, node, sent, serve_node, start_keeper, timers,
    wait_until,
};
use std::time::Duration;

const OLD_FACTORY: Address = Address::repeat_byte(0x67);
const OLD: Address = Address::repeat_byte(0x72);

/// The current factory's one vault never opened and is past its window: the
/// simplest thing it is owed is a cancel.
fn a_cancellable_vault(node: &process::Shared, now: u64) {
    answers(node, FACTORY, IVaultFactory::vaultsCall {}, vec![VAULT]);
    answers(
        node,
        FACTORY,
        IReplacesFactory::previousCall {},
        OLD_FACTORY,
    );
    answers(
        node,
        OLD_FACTORY,
        IReplacesFactory::previousCall {},
        Address::ZERO,
    );
    answers(node, VAULT, IVault::stateCall {}, VaultState::Activating);
    answers(node, VAULT, IVault::timersCall {}, timers());
    answers(
        node,
        VAULT,
        IVault::createdAtCall {},
        now - timers().activationWindow - 1,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_earlier_factorys_vault_with_a_short_slot_window_does_not_hold_up_the_current_vaults() {
    let now = 1_700_000_000;
    let node = node(now);
    a_cancellable_vault(&node, now);
    answers(&node, OLD_FACTORY, IVaultFactory::vaultsCall {}, vec![OLD]);
    let mut short = timers();
    short.slotWindow = 2;
    answers(&node, OLD, IVault::stateCall {}, VaultState::Activating);
    answers(&node, OLD, IVault::timersCall {}, short);
    answers(&node, OLD, IVault::createdAtCall {}, now);
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("short-window-earlier-vault", &rpc, "http://127.0.0.1:1");
    assert!(
        wait_until(Duration::from_secs(20), || {
            sent::<IVault::cancelCall>(&node, VAULT) != 0
        })
        .await,
        "the current factory's vault was not cancelled while an earlier factory's vault has a short slot window: {}",
        keeper.logged()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_earlier_factory_whose_list_cannot_be_read_does_not_hold_up_the_current_vaults() {
    let now = 1_700_000_000;
    let node = node(now);
    a_cancellable_vault(&node, now);
    // `vaults()` of the earlier factory is unscripted: the read fails, as it
    // does for good at a node whose `eth_call` gas cap the list exceeds.
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("unlisted-earlier-factory", &rpc, "http://127.0.0.1:1");
    assert!(
        wait_until(Duration::from_secs(20), || {
            sent::<IVault::cancelCall>(&node, VAULT) != 0
        })
        .await,
        "the current factory's vault was not cancelled while an earlier factory's list is unreadable: {}",
        keeper.logged()
    );
}
