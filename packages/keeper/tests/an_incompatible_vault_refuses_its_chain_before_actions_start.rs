//! Startup timer refusal applies to the whole affected chain, while another
//! connected chain can still act. Correcting the incompatible timers lets
//! the refused chain start without restarting the process.
mod process;

use chain::bindings::vault::{IVault, VaultState};
use process::{
    VAULT, answers, node, one_vault, sent, serve_node, start_keeper_on, timers, wait_until,
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn an_incompatible_vault_refuses_its_chain_before_actions_start() {
    let now = 1_700_000_000;
    let refused = node(now);
    let kept = node(now);
    kept.lock().unwrap().chain_id = 31338;
    for node in [&refused, &kept] {
        one_vault(node);
        answers(node, VAULT, IVault::stateCall {}, VaultState::Activating);
        answers(
            node,
            VAULT,
            IVault::createdAtCall {},
            now - timers().activationWindow - 1,
        );
        answers(node, VAULT, IVault::timersCall {}, timers());
    }
    let mut incompatible = timers();
    incompatible.slotWindow = 2;
    answers(&refused, VAULT, IVault::timersCall {}, incompatible);
    let refused_rpc = serve_node(refused.clone()).await;
    let kept_rpc = serve_node(kept.clone()).await;
    let keeper = start_keeper_on(
        "startup-timer-refusal",
        &[
            ("base", 31337, &refused_rpc),
            ("hyperliquid", 31338, &kept_rpc),
        ],
        "http://127.0.0.1:1",
    );
    assert!(
        wait_until(Duration::from_secs(15), || {
            sent::<IVault::cancelCall>(&kept, VAULT) != 0
                && keeper.logged().contains("slotWindow=2s")
        })
        .await,
        "the compatible chain did not act independently: {}",
        keeper.logged()
    );
    assert!(refused.lock().unwrap().sent.is_empty());
    assert!(keeper.logged().contains(&VAULT.to_string()));
    answers(&refused, VAULT, IVault::timersCall {}, timers());
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
async fn vault_startup_refusal_does_not_hold_up_independent_key_loops() {
    let node = node(1_700_000_000);
    one_vault(&node);
    let mut incompatible = timers();
    incompatible.slotWindow = 2;
    answers(&node, VAULT, IVault::timersCall {}, incompatible);
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
    let keeper =
        process::start_keeper_with_service("startup-independent-keys", &rpc, &urls[0], &urls[1]);
    assert!(
        wait_until(Duration::from_secs(10), || {
            executor_calls.load(Ordering::SeqCst) != 0
                && service_calls.load(Ordering::SeqCst) != 0
                && keeper.logged().contains("slotWindow=2s")
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
