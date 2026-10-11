//! Two chains connect; one signs for too few accounts to have a keeper's.
//! That chain is not kept, and said not to be in the keeper's health; the
//! other is kept all the same, and the keeper runs on.

mod process;

use std::time::Duration;

use chain::bindings::factory::IVaultFactory;
use chain::bindings::vault::IVault;
use process::{
    FACTORY, VAULT, answers, called, node, one_vault, serve_node, start_keeper_on, timers,
    wait_until,
};

#[tokio::test(flavor = "multi_thread")]
async fn a_chain_with_no_keeper_account_holds_up_no_other() {
    let kept = node(1_700_000_005);
    one_vault(&kept);
    answers(&kept, VAULT, IVault::timersCall {}, timers());
    // Timers permit startup; the remaining vault reads are unscripted.
    // Its round fails, and the chain's rounds still finish.
    let keyless = node(1_700_000_005);
    {
        let mut keyless = keyless.lock().unwrap();
        keyless.chain_id = 31338;
        keyless.accounts = 3;
    }
    one_vault(&keyless);
    let (kept_rpc, keyless_rpc) = (
        serve_node(kept.clone()).await,
        serve_node(keyless.clone()).await,
    );
    let mut keeper = start_keeper_on(
        "no-keeper-account",
        &[
            ("base", 31337, kept_rpc.as_str()),
            ("hyperliquid", 31338, keyless_rpc.as_str()),
        ],
        "http://127.0.0.1:1",
    );

    assert!(
        wait_until(Duration::from_secs(30), || {
            called::<IVaultFactory::vaultsCall>(&kept, FACTORY) >= 3
        })
        .await,
        "the chain with a keeper account was not kept: {}",
        keeper.logged()
    );
    let health = keeper
        .health_until(Duration::from_secs(20), |status| status == 503)
        .await;
    assert_eq!(keeper.exited(), None, "the keeper stopped");
    let (status, said) = health.clone().unwrap_or_default();
    assert_eq!(status, 503, "{health:?}");
    assert!(said.contains("hyperliquid (no keeper account)"), "{said:?}");
    assert!(!said.contains("base ("), "{said:?}");
    assert_eq!(called::<IVaultFactory::vaultsCall>(&keyless, FACTORY), 0);
}
