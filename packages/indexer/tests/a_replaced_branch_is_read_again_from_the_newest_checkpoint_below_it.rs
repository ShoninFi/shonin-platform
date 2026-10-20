//! A reorganisation replaces blocks the record has read. What the record
//! made of them (a vault's events, what a holder paid) is undone by going
//! back to the newest checkpoint at or below the fork, and the blocks after
//! it are read again from the branch the chain is on now: never a log of
//! the replaced branch left in the record, and never the whole history read
//! again for a shallow replacement. One deeper than the record can trace
//! (beyond the anchors it holds, or below every checkpoint kept) reads the
//! whole history again from the deployment.
//!
//! Here: one vault. On the first branch a holder deposits $100 at block 95;
//! the blocks above 90 are then replaced by a branch where the holder
//! deposits $40 at block 97.

mod stand_in;

use std::sync::{Arc, Mutex};

use alloy::primitives::{Address, U256};
use chain::bindings::vault::IVault;
use indexer::Indexer;
use serde_json::{Value, json};
use shared::ChainAddress;
use shared::model::activity::EventKind;
use stand_in::*;

const HOLDER: Address = Address::repeat_byte(0x22);

fn deposited(chain: &Chain, dollars: u64, block: u64) -> Value {
    chain.log(
        vault(),
        IVault::Deposited {
            requestId: U256::from(block),
            owner: HOLDER,
            assets: U256::from(dollars * 1_000_000),
            shares: U256::from(dollars) * U256::from(10u64.pow(18)),
        },
        block,
        0,
    )
}

/// An indexer of a chain whose logs are, on the first branch, a $100
/// deposit at block 95, and on any other a $40 deposit at block 97; and
/// the first block of every log read since it was last cleared.
async fn an_indexer_of_two_branches(
    chain: Arc<Chain>,
    policy: chain::rpc_policy::RpcPolicy,
    name: &str,
) -> (Arc<Indexer>, Arc<Mutex<Vec<u64>>>) {
    let reads: Arc<Mutex<Vec<u64>>> = Arc::default();
    let kept = reads.clone();
    let on = chain.clone();
    let answer: Answer = Arc::new(move |request: &Value| {
        if request["method"] != "eth_getLogs" {
            return None;
        }
        kept.lock().unwrap().push(log_range(request).0);
        let (_, branch) = *on.fork.lock().unwrap();
        let logs = if branch == 0 {
            vec![deposited(&on, 100, 95)]
        } else {
            vec![deposited(&on, 40, 97)]
        };
        Some(Ok(json!(matching(request, &logs))))
    });
    let node = a_node_on(chain, answer).await;
    let store = std::env::temp_dir().join(format!(
        "shonin-indexer-replaced-{name}-{}",
        std::process::id()
    ));
    let indexer =
        Arc::new(Indexer::new(vec![shared::Chain::Base], store, None).with_rpc_policy(policy));
    indexer
        .add(chain::Served {
            vault_chain: shared::Chain::Base,
            key: "evm:31337".parse().unwrap(),
            client: client(&node).await,
            deployments: deployments(),
            native_usd: None,
            public_rpc: String::new(),
        })
        .await;
    (indexer, reads)
}

/// The deposits the record shows, in dollars, newest first, and what the
/// holder paid as the record has it.
async fn deposits_and_cost(indexer: &Indexer) -> (Vec<f64>, Option<f64>) {
    let snapshot = indexer.snapshot.read().await.clone();
    let deposits = snapshot.strategies[0]
        .events
        .iter()
        .filter(|event| event.kind == EventKind::Deposited)
        .filter_map(|event| event.amount)
        .collect();
    let cost = indexer
        .cost(
            shared::Chain::Base,
            &ChainAddress::Evm(vault().0.0),
            &ChainAddress::Evm(HOLDER.0.0),
        )
        .await;
    (deposits, cost)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shallow_replacement_is_undone_to_the_newest_checkpoint_below_it() {
    let chain = Chain::at(80);
    let (indexer, reads) = an_indexer_of_two_branches(chain.clone(), policy(), "shallow").await;
    indexer.refresh().await.unwrap();
    chain.head.store(100, std::sync::atomic::Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    assert_eq!(
        deposits_and_cost(&indexer).await,
        (vec![100.0], Some(100.0)),
        "the first branch's deposit is read"
    );

    // Every block above 90 is replaced, ten blocks below the record's head.
    chain.fork(90, 1, 110);
    reads.lock().unwrap().clear();
    indexer.refresh().await.unwrap();
    let after = deposits_and_cost(&indexer).await;
    let read_from = reads.lock().unwrap().clone();
    assert_eq!(
        after,
        (vec![40.0], Some(40.0)),
        "after the replacement the record shows {after:?}"
    );
    assert!(
        !read_from.is_empty() && read_from.iter().all(|from| *from > DEPLOYED + 16),
        "a replacement ten blocks deep read logs again from blocks {read_from:?}"
    );
    let info = indexer.snapshot.read().await.chains[0].clone();
    assert_eq!((info.block, info.catching_up), (110, false));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replacement_deeper_than_the_anchors_reach_reads_everything_again() {
    let shallow = chain::rpc_policy::RpcPolicy {
        max_reorg_blocks: 16,
        record_overlap_blocks: 16,
        ..policy()
    };
    let chain = Chain::at(100);
    let (indexer, reads) = an_indexer_of_two_branches(chain.clone(), shallow, "deep").await;
    indexer.refresh().await.unwrap();
    chain.head.store(200, std::sync::atomic::Ordering::SeqCst);
    indexer.refresh().await.unwrap();
    assert_eq!(
        deposits_and_cost(&indexer).await,
        (vec![100.0], Some(100.0))
    );

    // Every block above 90 is replaced: further below the head than the
    // sixteen blocks of anchors the record holds.
    chain.fork(90, 1, 210);
    reads.lock().unwrap().clear();
    indexer.refresh().await.unwrap();
    let after = deposits_and_cost(&indexer).await;
    let read_from = reads.lock().unwrap().clone();
    assert_eq!(after, (vec![40.0], Some(40.0)), "{after:?}");
    assert!(
        read_from.contains(&DEPLOYED),
        "a replacement deeper than the anchors read logs again only from blocks {read_from:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replacement_below_every_checkpoint_kept_reads_everything_again() {
    let few = chain::rpc_policy::RpcPolicy {
        record_checkpoints: 2,
        ..policy()
    };
    let chain = Chain::at(100);
    let (indexer, reads) =
        an_indexer_of_two_branches(chain.clone(), few, "below-checkpoints").await;
    for head in [100, 120, 140] {
        chain.head.store(head, std::sync::atomic::Ordering::SeqCst);
        indexer.refresh().await.unwrap();
    }
    assert_eq!(
        deposits_and_cost(&indexer).await,
        (vec![100.0], Some(100.0))
    );

    // The checkpoints kept are at 120 and 140; the fork is below both.
    chain.fork(90, 1, 150);
    reads.lock().unwrap().clear();
    indexer.refresh().await.unwrap();
    let after = deposits_and_cost(&indexer).await;
    let read_from = reads.lock().unwrap().clone();
    assert_eq!(after, (vec![40.0], Some(40.0)), "{after:?}");
    assert!(
        read_from.contains(&DEPLOYED),
        "a replacement below every checkpoint read logs again only from blocks {read_from:?}"
    );
}
