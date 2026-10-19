//! On a local Solana cluster with the keeper running, a deposit is valued in
//! the first slot whose valuation prices it, inside that slot's window, and
//! settled after it, as the keeper always has; and the keeper does so
//! reading the cluster at its wakes: one read of every account a look,
//! answered as of one slot, between looks nothing.

mod solana_world;

use std::time::Duration;

use chain::solana::programs::vault::state::{self as vs, Request, Vault};
use solana_world::{DEPOSIT, World, this_keeper};

#[tokio::test(flavor = "multi_thread")]
async fn a_deposit_is_valued_in_the_slot_that_prices_it_and_settled_after_reading_the_cluster_at_its_wakes()
 {
    let Some(world) = World::new("deposit", "").await else {
        return;
    };
    let (_, v) = world.vault().await;
    let keeper = world.keeper(&this_keeper(), "candidate", "info,keeper::solana=debug");
    assert!(
        world
            .until(Duration::from_secs(120), || async {
                keeper.said().contains("the cluster is read at its wakes")
            })
            .await,
        "the keeper never read the cluster at its wakes:\n{}",
        keeper.said()
    );
    world.deposit(&v, DEPOSIT, 1).await;
    let viewer = world.role(chain::Role::Viewer);
    let request_at = vs::request_address(&v.vault, &chain::solana::Signer::pubkey(viewer), 1).0;
    let request: Request = world.p.client.load(&request_at).await.unwrap().unwrap();
    assert!(
        world
            .until(Duration::from_secs(90), || async {
                world.shares(&v).await != 0
            })
            .await,
        "the deposit was never priced:\n{}",
        keeper.said()
    );
    let vault: Vault = world.p.client.load(&v.vault).await.unwrap().unwrap();
    let sends = world.sends(&[v.vault]).await;
    let valued: Vec<_> = sends
        .iter()
        .filter(|sent| sent.is("value", &v.vault))
        .collect();
    let interval = vault.timers.slot_interval;
    let first_slot = request.earliest.div_ceil(interval) * interval;
    let pricing = valued
        .iter()
        .find(|sent| sent.time as u64 >= request.earliest)
        .expect("no valuation after the request");
    assert_eq!(
        pricing.time as u64 / interval * interval,
        first_slot,
        "the deposit was priced in another slot than its first: {sends:#?}"
    );
    assert!(
        (pricing.time as u64) < first_slot + vault.timers.slot_window,
        "the valuation landed outside its window: {sends:#?}"
    );
    assert!(
        sends
            .iter()
            .any(|sent| sent.is("settle", &v.vault) && sent.slot > pricing.slot),
        "the deposit was not settled after its valuation: {sends:#?}"
    );
    drop(keeper);
}
