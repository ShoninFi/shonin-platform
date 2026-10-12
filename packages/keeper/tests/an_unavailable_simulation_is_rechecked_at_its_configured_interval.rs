mod process;

use alloy::primitives::Address;
use chain::bindings::factory::IVaultFactory;
use process::{
    FACTORY, answers, called, node, serve_node, simulation_check, start_keeper_with_rpc_policy,
    wait_until,
};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[tokio::test(flavor = "multi_thread")]
async fn an_unavailable_simulation_is_rechecked_at_its_configured_interval() {
    for (initial, maximum, factor) in [
        (1_000, 2_000, 2),
        (1_000, 4_000, 1),
        (1_000, 1_000, 2),
        (1_250, 3_000, 2),
    ] {
        let node = node(1_700_000_005);
        answers(
            &node,
            FACTORY,
            IVaultFactory::vaultsCall {},
            Vec::<Address>::new(),
        );
        let ready = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(Mutex::new(Vec::<Instant>::new()));
        let ready_at_node = ready.clone();
        let recorded = attempts.clone();
        node.lock().unwrap().on_request = Some(Arc::new(move |method, params| {
            if method != "eth_call"
                || !simulation_check(params)
                || ready_at_node.load(Ordering::SeqCst)
            {
                return None;
            }
            if params[0].get("to").is_none_or(serde_json::Value::is_null) {
                recorded.lock().unwrap().push(Instant::now());
            }
            Some(json!({"code":3,"message":"execution reverted"}))
        }));
        let rpc = serve_node(node.clone()).await;
        let keeper = start_keeper_with_rpc_policy(
            &format!("simulation-{initial}-{maximum}-{factor}"),
            &rpc,
            "http://127.0.0.1:1",
            &[
                ("keeper_simulation_initial_ms", initial),
                ("keeper_simulation_max_ms", maximum),
                ("keeper_simulation_backoff_factor", factor),
            ],
        );
        assert!(
            wait_until(Duration::from_secs(15), || attempts.lock().unwrap().len()
                >= 5)
            .await,
            "no configured rechecks: {}",
            keeper.logged()
        );
        let attempts = attempts.lock().unwrap().clone();
        let mut expected = initial;
        for pair in attempts.windows(2).take(4) {
            let elapsed = pair[1].duration_since(pair[0]);
            assert!(
                elapsed >= Duration::from_millis(expected),
                "early recheck: {elapsed:?}, expected {expected}ms"
            );
            assert!(
                elapsed < Duration::from_millis(expected + 750),
                "late recheck: {elapsed:?}, expected {expected}ms"
            );
            expected = (expected * factor).min(maximum);
        }
        assert_eq!(
            called::<IVaultFactory::vaultsCall>(&node, FACTORY),
            0,
            "vault startup ran before simulation became available"
        );
        let recovering = Instant::now();
        ready.store(true, Ordering::SeqCst);
        assert!(
            wait_until(Duration::from_secs(15), || called::<
                IVaultFactory::vaultsCall,
            >(&node, FACTORY)
                > 0)
            .await,
            "startup never resumed: {}",
            keeper.logged()
        );
        assert!(
            recovering.elapsed() < Duration::from_millis(maximum + 750),
            "recovery exceeded configured maximum"
        );
    }
}
