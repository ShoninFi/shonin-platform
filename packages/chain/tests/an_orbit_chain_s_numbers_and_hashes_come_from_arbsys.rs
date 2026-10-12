//! Where ArbSys answers at `0x64` (an Arbitrum Orbit chain, whose
//! `block.number` inside a call is the parent chain's), the follower takes
//! the block number and block hashes from it, on the helper's path and on
//! the Multicall3 path alike, and follows the branch as anywhere else. A
//! stand-in ArbSys is put at `0x64` on a local node; the helper's own
//! handling of the parent chain's numbers is shown by its forge tests.

mod stand_in_node;

use alloy::primitives::Address;
use chain::follow::{At, BlockSource, Continuity, Path, Plan};
use stand_in_node::{Anvil, Proxy, policy};

const ARB_SYS: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x64,
]);

/// A chain of 401 blocks with ArbSys and Multicall3 in place from block
/// 398, and a snapshot there to fork from.
async fn orbit(id: u64) -> (Anvil, serde_json::Value) {
    let anvil = Anvil::start(id, 1_700_000_000).await;
    anvil.mine(397, 1).await;
    anvil
        .set_code(ARB_SYS, &stand_in_node::code("MockArbSys"))
        .await;
    anvil
        .set_code(
            stand_in_node::MULTICALL,
            &stand_in_node::code("StandInMulticall3"),
        )
        .await;
    anvil.mine(1, 1).await;
    let at_398 = anvil.snapshot().await;
    anvil.mine(3, 1).await;
    (anvil, at_398)
}

#[tokio::test]
async fn the_helper_takes_numbers_and_hashes_from_arbsys_and_follows_the_branch() {
    let (anvil, at_398) = orbit(31_470_001).await;
    let mut follower = stand_in_node::follower(&[&anvil.url], policy()).await;
    let plan = Plan::default();
    let step = follower.step(&plan, At::Latest).await;
    assert_eq!(step.continuity, Continuity::Extends);
    assert_eq!(
        follower.capabilities(0).unwrap().source,
        Some(BlockSource::ArbSys)
    );
    let observed = step.observed.unwrap();
    assert_eq!(observed.path, Path::ViewReads);
    assert_eq!(observed.number, anvil.head().await);
    assert_eq!(observed.hashes.len(), 256);
    for (number, hash) in &observed.hashes {
        assert_eq!(anvil.block(*number).await.0, *hash);
    }
    // The newest three blocks replaced: the fork is two below the newest
    // anchor (the head's parent, 400).
    anvil.fork_at(&at_398).await;
    anvil.mine(5, 2).await;
    assert_eq!(
        follower.step(&plan, At::Latest).await.continuity,
        Continuity::Replaced {
            fork_at: 398,
            depth: 2
        }
    );
}

#[tokio::test]
async fn multicall3_takes_numbers_and_hashes_from_arbsys_too() {
    let (anvil, _) = orbit(31_470_002).await;
    let proxy = Proxy::start(&anvil.url).await;
    proxy.script(|script| script.refuse_overrides = true);
    let mut follower = stand_in_node::follower(&[&proxy.url], policy()).await;
    let step = follower.step(&Plan::default(), At::Latest).await;
    assert_eq!(step.continuity, Continuity::Extends);
    let observed = step.observed.unwrap();
    assert_eq!(observed.path, Path::Multicall3);
    assert_eq!(
        follower.capabilities(0).unwrap().source,
        Some(BlockSource::ArbSys)
    );
    for (number, hash) in &observed.hashes {
        assert_eq!(anvil.block(*number).await.0, *hash);
    }
    assert_eq!(observed.hashes.len(), 257);
}
