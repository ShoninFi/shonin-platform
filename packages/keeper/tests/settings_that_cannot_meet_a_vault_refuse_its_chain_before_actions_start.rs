//! RPC settings slower than the established ones that cannot meet a vault's
//! timers refuse that chain's vault rounds at startup, naming the vault and
//! the timer, while another connected chain still acts and the key loops
//! still run. Correcting the timers lets the refused chain start without
//! restarting the process. A vault whose own timers are shorter than even the
//! established cadence can guarantee is still kept.
mod process;

use chain::bindings::vault::{IVault, Timers, VaultState};
use process::{
    VAULT, answers, node, one_vault, sent, serve_node, start_keeper, start_keeper_configured,
    timers, wait_until,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

/// Slower than the established read budget: a lead of 8 s and an active
/// margin of 6 s, which the stand-in's 5 s lead and 3 s window can't meet;
/// the lookahead covers the whole margin, as the settings require.
const SLOWER: &[(&str, u64)] = &[("read_budget_ms", 4_000), ("due_lookahead_ms", 8_000)];

/// Timers roomy enough for [`SLOWER`].
fn roomy() -> Timers {
    let mut roomy = timers();
    roomy.slotInterval = 20;
    roomy.slotLead = 9;
    roomy.slotWindow = 7;
    roomy
}

fn cancellable(node: &process::Shared, now: u64, timers: Timers) {
    one_vault(node);
    answers(node, VAULT, IVault::stateCall {}, VaultState::Activating);
    answers(
        node,
        VAULT,
        IVault::createdAtCall {},
        now - timers.activationWindow - 1,
    );
    answers(node, VAULT, IVault::timersCall {}, timers);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn settings_that_cannot_meet_a_vault_refuse_its_chain_before_actions_start() {
    let now = 1_700_000_000;
    let refused = node(now);
    let kept = node(now);
    kept.lock().unwrap().chain_id = 31338;
    cancellable(&refused, now, timers());
    cancellable(&kept, now, roomy());
    let refused_rpc = serve_node(refused.clone()).await;
    let kept_rpc = serve_node(kept.clone()).await;
    let keeper = start_keeper_configured(
        "startup-timer-refusal",
        &[
            ("base", 31337, &refused_rpc),
            ("hyperliquid", 31338, &kept_rpc),
        ],
        "http://127.0.0.1:1",
        &[],
        SLOWER,
    );
    assert!(
        wait_until(Duration::from_secs(15), || {
            sent::<IVault::cancelCall>(&kept, VAULT) != 0 && keeper.logged().contains("slotLead=5s")
        })
        .await,
        "the compatible chain did not act independently: {}",
        keeper.logged()
    );
    assert!(refused.lock().unwrap().sent.is_empty());
    assert!(keeper.logged().contains(&VAULT.to_string()));
    assert!(keeper.logged().contains("read_budget_ms=4000"));
    answers(&refused, VAULT, IVault::timersCall {}, roomy());
    assert!(
        wait_until(Duration::from_secs(15), || {
            sent::<IVault::cancelCall>(&refused, VAULT) != 0
        })
        .await,
        "the corrected chain did not start: {}",
        keeper.logged()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn a_vault_shorter_than_the_established_cadence_is_still_kept() {
    let now = 1_700_000_000;
    let node = node(now);
    let mut short = timers();
    short.slotWindow = 2;
    cancellable(&node, now, short);
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper("startup-short-timers-kept", &rpc, "http://127.0.0.1:1");
    assert!(
        wait_until(Duration::from_secs(15), || {
            sent::<IVault::cancelCall>(&node, VAULT) != 0
                && keeper.logged().contains("slotWindow=2s")
        })
        .await,
        "a vault with its own short window was not kept, or not said: {}",
        keeper.logged()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn vault_startup_refusal_does_not_hold_up_independent_key_loops() {
    let node = node(1_700_000_000);
    one_vault(&node);
    answers(&node, VAULT, IVault::timersCall {}, timers());
    let executor_calls = Arc::new(AtomicU64::new(0));
    let service_calls = Arc::new(AtomicU64::new(0));
    let mut urls = Vec::new();
    for counted in [&executor_calls, &service_calls] {
        let counted = counted.clone();
        urls.push(
            process::serve(axum::Router::new().route(
                "/attestation",
                axum::routing::get(move || {
                    let counted = counted.clone();
                    async move {
                        counted.fetch_add(1, Ordering::SeqCst);
                        axum::http::StatusCode::NOT_FOUND
                    }
                }),
            ))
            .await,
        );
    }
    let rpc = serve_node(node.clone()).await;
    let keeper = start_keeper_configured(
        "startup-independent-keys",
        &[("base", 31337, &rpc)],
        &urls[0],
        &["--service-attestation-urls", &urls[1]],
        SLOWER,
    );
    assert!(
        wait_until(Duration::from_secs(10), || {
            executor_calls.load(Ordering::SeqCst) != 0
                && service_calls.load(Ordering::SeqCst) != 0
                && keeper.logged().contains("slotLead=5s")
        })
        .await,
        "vault refusal held up a key loop: {}",
        keeper.logged()
    );
    assert!(node.lock().unwrap().sent.is_empty());
    let (status, message) = keeper.health().await.unwrap();
    assert_eq!(status, 503);
    assert!(message.contains("vault startup checks"));
}
