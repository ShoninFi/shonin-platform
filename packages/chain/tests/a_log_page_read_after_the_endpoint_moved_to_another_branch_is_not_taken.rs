//! A page of logs is taken only when its end is shown to be on the followed
//! branch, as the endpoint that answered the page gives that end just after
//! the logs. That holds for the endpoint the observation came from too:
//! between the observation and the page read it may have moved to another
//! branch (a replacement at its tip), and the page then carries that
//! branch's logs: a log the followed branch has is simply absent, and no
//! log-bearing block is left to compare. Such a page must not be taken as
//! the followed branch's logs through its end.

mod stand_in_node;

use alloy::primitives::U256;
use alloy_sol_types::{SolCall, SolEvent};
use chain::follow::{At, Plan, RangeRead, RangeReader, Watch};
use stand_in_node::{Anvil, FollowProbe, PROBE, policy};

fn watch() -> Watch {
    Watch {
        addresses: vec![PROBE],
        topics: vec![FollowProbe::Noted::SIGNATURE_HASH],
    }
}

#[tokio::test]
async fn a_log_page_read_after_the_endpoint_moved_to_another_branch_is_not_taken() {
    let anvil = Anvil::start(31_490_001, 1_700_000_000).await;
    anvil.mine(300, 1).await;
    stand_in_node::put_probe(&anvil, PROBE, 1).await;
    anvil.mine(10, 1).await;
    let fork = anvil.snapshot().await;
    let fork_at = anvil.head().await;
    // The followed branch: a log in the block after the fork, then more.
    anvil
        .send(
            PROBE,
            FollowProbe::noteCall {
                count: U256::from(1),
            }
            .abi_encode(),
        )
        .await;
    let noted_at = anvil.head().await;
    assert_eq!(noted_at, fork_at + 1);
    anvil.mine(10, 1).await;
    let head = anvil.head().await;

    let mut follower = stand_in_node::follower(&[&anvil.url], policy()).await;
    let mut reader = RangeReader::new(watch(), 1, &policy());
    // The observation proves the branch with the log, at the head.
    let step = follower.step(&Plan::default(), At::Latest).await;
    let observed = step.observed.expect("an observation");
    assert_eq!(observed.number, head);

    // Before the page is read, the endpoint replaces every block above the
    // fork: the same height, other blocks, and no log.
    anvil.fork_at(&fork).await;
    anvil.mine(11, 1).await;
    assert_eq!(anvil.head().await, head);
    assert_ne!(
        observed.hashes.get(&noted_at).copied(),
        Some(anvil.block(noted_at).await.0),
        "the endpoint is on another branch above the fork"
    );

    let pages = reader
        .pages(
            &follower,
            head - 1,
            Some((observed.endpoint, observed.number)),
        )
        .await
        .unwrap()
        .expect("pages to read");
    let settled = reader.settle(pages, &mut follower, &observed).await;
    // What the next whole read makes of it, for the record.
    anvil.mine(1, 1).await;
    let next = reader.read(&mut follower, None).await;
    eprintln!("the page read after the move: {settled:?}\nthe read after it: {next:?}");
    match settled {
        RangeRead::Read { logs, through, .. } => {
            assert!(
                logs.iter().any(|log| log.block_number == Some(noted_at)),
                "the read says every block through {through} was read on the followed branch, \
                 and has no log for block {noted_at}, which that branch has: {logs:?}"
            );
        }
        // Not checked, nothing taken: acceptable.
        RangeRead::Unverified(_) => {}
        other => panic!("{other:?}"),
    }
}
