//! Each chain's part of the record is read and written on its own: one
//! connected chain whose node has turned slow (not down), each request
//! taking seconds and a read dozens of them, holds back no other chain's
//! blocks, vaults and prices in the snapshot.
//!
//! Sequence: base and hyperliquid connect; hyperliquid's node then answers
//! each request after 8 s; base's answers at once. Within 20 s base's block
//! is in the snapshot, dated on its own clock.

mod stand_in;

use std::sync::Arc;
use std::time::Duration;

use chain::Served;
use indexer::Indexer;
use stand_in::Chain;

/// `vault_chain`, on a node of chain `chain`, at block 0x10, with no vaults
/// and no logs.
async fn served(vault_chain: shared::Chain, chain: Arc<Chain>) -> Served {
    let id = chain.id;
    stand_in::served_as(
        vault_chain,
        chain,
        Duration::ZERO,
        stand_in::deployments_on(id, 0),
        stand_in::answering_no_vaults(),
    )
    .await
}

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shonin-indexer-slow-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Base's block in the snapshot after 20 s of reading, and whether it is
/// dated, hyperliquid's node slow or not.
async fn base_block_after_20s(hyperliquid_slow: bool) -> Option<(u64, bool)> {
    let indexer = Arc::new(
        Indexer::new(
            vec![shared::Chain::Base, shared::Chain::Hyperliquid],
            scratch(),
            None,
        )
        .with_rpc_policy(chain::rpc_policy::RpcPolicy {
            record_refresh_ms: 2_000,
            ..stand_in::policy()
        }),
    );
    let fast = Chain::numbered(31_337, stand_in::HEAD);
    let slow = Chain::numbered(31_338, stand_in::HEAD);
    indexer.add(served(shared::Chain::Base, fast).await).await;
    indexer
        .add(served(shared::Chain::Hyperliquid, slow.clone()).await)
        .await;
    // Connected; now hyperliquid's node slows down.
    if hyperliquid_slow {
        slow.delay_ms
            .store(8_000, std::sync::atomic::Ordering::SeqCst);
    }
    let running = indexer.clone();
    let reading = tokio::spawn(async move { running.run().await });

    tokio::time::sleep(Duration::from_secs(20)).await;
    let block = indexer
        .snapshot
        .read()
        .await
        .chain(shared::Chain::Base)
        .map(|info| (info.block, info.as_of.is_some()));
    reading.abort();
    block
}

/// With both nodes answering at once, base's block is read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_both_nodes_quick_base_is_read() {
    assert_eq!(base_block_after_20s(false).await, Some((16, true)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_slow_chain_does_not_hold_back_another_chain_s_record() {
    let block = base_block_after_20s(true).await;
    assert_eq!(
        block,
        Some((16, true)),
        "after 20 s base's record still reads {block:?}: hyperliquid's slow node held it back"
    );
}
