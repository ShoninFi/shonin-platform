//! A live vault whose station has its key approved, and whose key the
//! registry still blesses: the station refuses `revokeKey` for it, so the
//! keeper never sends one, nor asks its gas, round after round. It reads
//! the blessing itself to know, as it does whichever way it reads the chain.

mod process;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use chain::bindings::attestation::IAttestationRegistry;
use chain::bindings::station::{IStation, IStationVault};
use chain::bindings::vault::{IVault, VaultState};
use process::{
    REGISTRY, STATION, VAULT, a_registry, a_spec, a_tank, answers, answers_exactly, called,
    count_estimates, name, node, one_vault, sent, serve_node, start_keeper, timers, wait_until,
};

#[tokio::test(flavor = "multi_thread")]
async fn a_live_vault_s_key_still_blessed_is_never_sent_a_revoke() {
    let node = node(1_700_000_005);
    one_vault(&node);
    let local = name("local");
    let key = Address::repeat_byte(0x77);
    answers(&node, VAULT, IVault::stateCall {}, VaultState::Open);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    answers(&node, VAULT, IVault::tankCall {}, a_tank(50_000_000));
    answers(&node, VAULT, IStationVault::haltedAtCall {}, 0);
    answers(&node, VAULT, IVault::homeStationCall {}, STATION);
    answers(
        &node,
        VAULT,
        IVault::unitsCall {},
        IVault::unitsReturn {
            total: U256::ZERO,
            reserve: U256::ZERO,
        },
    );
    answers(&node, VAULT, IVault::valuationCountCall {}, U256::ZERO);
    answers(&node, VAULT, IStationVault::unsettledCall {}, false);
    answers(&node, VAULT, IVault::claimCountCall {}, U256::ZERO);
    answers(&node, STATION, IStation::specCall {}, a_spec(vec![local]));
    answers_exactly(
        &node,
        STATION,
        IStation::approvedKeyCall { driver: local },
        key,
    );
    a_registry(&node, local, key, true);
    let estimated = Arc::new(Mutex::new(0));
    count_estimates(
        &node,
        STATION,
        IStation::revokeKeyCall::SELECTOR,
        estimated.clone(),
        None,
    );
    let rpc = serve_node(node.clone()).await;
    let _keeper = start_keeper("still-blessed", &rpc, "http://127.0.0.1:1");

    // Four rounds.
    assert!(
        wait_until(Duration::from_secs(30), || {
            called::<IVault::stateCall>(&node, VAULT) >= 4
        })
        .await
    );
    assert_eq!(
        sent::<IStation::revokeKeyCall>(&node, STATION),
        0,
        "a revoke of a key still blessed was sent"
    );
    assert_eq!(
        *estimated.lock().unwrap(),
        0,
        "a revoke of a key still blessed had its gas estimated"
    );
    assert!(
        called::<IAttestationRegistry::isBlessedForCall>(&node, REGISTRY) >= 3,
        "the keeper did not read the key's blessing round after round"
    );
}
