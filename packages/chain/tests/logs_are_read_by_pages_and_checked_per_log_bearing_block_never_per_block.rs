//! Logs are read by ranges: one log query per page over every watched
//! address, in pages of the configured width, and checked against the
//! followed branch per page and per log-bearing block, never per block. The
//! read is complete (every log a direct query finds, once, in order), stays
//! complete under an endpoint's range cap, finds a log an answer left out
//! by re-reading the overlap, and starts again from the fork when the
//! branch is replaced.

mod stand_in_node;

use alloy::primitives::{Address, U256};
use alloy::rpc::types::Log;
use alloy_sol_types::{SolCall, SolEvent};
use chain::follow::{RangeRead, RangeReader, Watch};
use serde_json::json;
use stand_in_node::{Anvil, FollowProbe, PROBE, PROBE_TWO, Proxy, policy};

fn watch() -> Watch {
    Watch {
        addresses: vec![PROBE, PROBE_TWO],
        topics: vec![FollowProbe::Noted::SIGNATURE_HASH],
    }
}

fn reading_policy() -> chain::rpc_policy::RpcPolicy {
    let mut policy = policy();
    policy.record_page_blocks = 200;
    policy.record_pages_per_refresh = 3;
    policy.record_overlap_blocks = 8;
    policy
}

async fn note(anvil: &Anvil, at: Address, count: u64) {
    anvil
        .send(
            at,
            FollowProbe::noteCall {
                count: U256::from(count),
            }
            .abi_encode(),
        )
        .await;
}

/// A chain of about 1,500 blocks with logs in a few of them.
async fn chain(id: u64) -> Anvil {
    let anvil = Anvil::start(id, 1_700_000_000).await;
    anvil.mine(50, 1).await;
    stand_in_node::put_probe(&anvil, PROBE, 1).await;
    stand_in_node::put_probe(&anvil, PROBE_TWO, 2).await;
    for (gap, at, count) in [
        (40, PROBE, 2),
        (0, PROBE_TWO, 1),
        (600, PROBE, 3),
        (500, PROBE_TWO, 2),
        (300, PROBE, 1),
    ] {
        anvil.mine(gap, 1).await;
        note(&anvil, at, count).await;
    }
    anvil.mine(4, 1).await;
    anvil
}

/// Every log a direct query of the node finds up to `to`, as (block,
/// index, address).
async fn direct(anvil: &Anvil, to: u64) -> Vec<(u64, u64, Address)> {
    let logs = anvil
        .rpc(
            "eth_getLogs",
            json!([{"fromBlock": "0x1", "toBlock": format!("0x{to:x}"), "address": [PROBE, PROBE_TWO]}]),
        )
        .await
        .unwrap();
    let logs: Vec<Log> = serde_json::from_value(logs).unwrap();
    logs.iter().map(key).collect()
}

fn key(log: &Log) -> (u64, u64, Address) {
    (
        log.block_number.unwrap(),
        log.log_index.unwrap(),
        log.address(),
    )
}

/// Read until caught up; every log taken, and how many reads it took.
async fn read_all(
    reader: &mut RangeReader,
    follower: &mut chain::follow::Follower,
) -> (Vec<(u64, u64, Address)>, u32) {
    let mut taken = Vec::new();
    for reads in 1..100 {
        match reader.read(follower, None).await {
            RangeRead::Read {
                logs, caught_up, ..
            } => {
                taken.extend(logs.iter().map(key));
                if caught_up {
                    return (taken, reads);
                }
            }
            other => panic!("read {reads}: {other:?}"),
        }
    }
    panic!("never caught up");
}

#[tokio::test]
async fn a_history_is_read_whole_by_pages_with_no_header_per_block() {
    let anvil = chain(31_450_001).await;
    let proxy = Proxy::start(&anvil.url).await;
    let mut follower = stand_in_node::follower(&[&proxy.url], reading_policy()).await;
    follower
        .observe(&Default::default(), chain::follow::At::Latest)
        .await
        .unwrap();
    proxy.forget_seen();
    let mut reader = RangeReader::new(watch(), 1, &reading_policy());
    let (taken, reads) = read_all(&mut reader, &mut follower).await;
    let head = anvil.head().await;
    assert_eq!(taken, direct(&anvil, head - 1).await);
    assert_eq!(taken.len(), 9);
    // 600 blocks a read: three reads for the whole chain.
    assert_eq!(reads, 3);
    let seen = proxy.seen();
    // One log query per page.
    let pages = (head - 1).div_ceil(200);
    assert_eq!(seen.count("eth_getLogs"), pages);
    // Headers: at most one per page end and per log-bearing block outside
    // the observations' windows, far fewer than one per block.
    let headers = seen.count("eth_getBlockByNumber");
    assert!(headers <= pages + 5, "{headers} headers for {head} blocks");
    // No head read per page: the observation's head is used.
    assert_eq!(seen.count("eth_blockNumber"), 0);
}

#[tokio::test]
async fn a_capped_log_range_is_narrowed_and_the_read_stays_complete() {
    let anvil = chain(31_450_002).await;
    let proxy = Proxy::start(&anvil.url).await;
    proxy.script(|script| script.log_range_cap = Some(50));
    let mut follower = stand_in_node::follower(&[&proxy.url], reading_policy()).await;
    let mut reader = RangeReader::new(watch(), 1, &reading_policy());
    let (taken, _) = read_all(&mut reader, &mut follower).await;
    assert_eq!(taken, direct(&anvil, anvil.head().await - 1).await);
}

#[tokio::test]
async fn a_log_left_out_once_is_found_by_the_overlap() {
    let anvil = chain(31_450_003).await;
    let proxy = Proxy::start(&anvil.url).await;
    let mut follower = stand_in_node::follower(&[&proxy.url], reading_policy()).await;
    let mut reader = RangeReader::new(watch(), 1, &reading_policy());
    let (mut taken, _) = read_all(&mut reader, &mut follower).await;
    // Two new logs; the answer that first covers them leaves one out.
    note(&anvil, PROBE_TWO, 2).await;
    let noted_at = anvil.head().await;
    anvil.mine(1, 1).await;
    proxy.script(|script| script.drop_a_log_once = true);
    let RangeRead::Read { logs, .. } = reader.read(&mut follower, None).await else {
        panic!("the first read of the new block");
    };
    assert_eq!(logs.len(), 1);
    taken.extend(logs.iter().map(key));
    // The next read re-reads the overlap and finds the other one.
    anvil.mine(1, 1).await;
    assert_eq!(
        reader.read(&mut follower, None).await,
        RangeRead::Replaced {
            fork_at: noted_at - 1
        }
    );
    // The caller forgets what it took above the fork, and reads it again.
    taken.retain(|(block, _, _)| *block < noted_at);
    let (again, _) = read_all(&mut reader, &mut follower).await;
    taken.extend(again);
    assert_eq!(taken, direct(&anvil, anvil.head().await - 1).await);
}

#[tokio::test]
async fn a_replaced_branch_is_read_again_from_its_fork() {
    let anvil = chain(31_450_004).await;
    let fork = anvil.head().await;
    let at_fork = anvil.snapshot().await;
    note(&anvil, PROBE_TWO, 1).await;
    anvil.mine(4, 1).await;
    let proxy = Proxy::start(&anvil.url).await;
    let mut follower = stand_in_node::follower(&[&proxy.url], reading_policy()).await;
    let mut reader = RangeReader::new(watch(), 1, &reading_policy());
    let (mut taken, _) = read_all(&mut reader, &mut follower).await;
    assert!(taken.iter().any(|(block, _, _)| *block == fork + 1));
    // The blocks above the fork are replaced by a longer branch with
    // another log in it.
    anvil.fork_at(&at_fork).await;
    note(&anvil, PROBE, 4).await;
    anvil.mine(8, 2).await;
    match reader.read(&mut follower, None).await {
        RangeRead::Replaced { fork_at } => {
            assert_eq!(fork_at, fork);
            taken.retain(|(block, _, _)| *block <= fork_at);
        }
        other => panic!("{other:?}"),
    }
    let (again, _) = read_all(&mut reader, &mut follower).await;
    taken.extend(again);
    assert_eq!(taken, direct(&anvil, anvil.head().await - 1).await);
}
