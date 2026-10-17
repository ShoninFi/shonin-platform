//! A reorganisation undoes blocks the keepers' sends landed in: each send
//! undone is made again at its own pace, none that stayed landed is made
//! again, and the candidate's canonical sends are `main`'s. Shallow (three
//! blocks, undoing a valuation and its settle) at two-second blocks; deep
//! (300 blocks, past the 256 a `blockhash` reaches) at quarter-second blocks.
//! The blocks mined after it are stamped with the wall's time again, as a
//! chain's are. See `differential/mod.rs`.

mod differential;

use differential::node::BlockTime;
use differential::settings::Profile;
use differential::{Fault, differs_no_more_than_the_baseline_with as check};

/// Three blocks back at t0+26: the valuation and settle of the slot at t0+20.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn three_blocks_deep() {
    check(
        BlockTime::TwoSeconds,
        Profile::checked_in(),
        &[Fault::Reorg { at: 26, depth: 3 }],
    )
    .await;
}

/// Three hundred blocks back at t0+100.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn three_hundred_blocks_deep() {
    check(
        BlockTime::QuarterSecond,
        Profile::checked_in(),
        &[Fault::Reorg {
            at: 100,
            depth: 300,
        }],
    )
    .await;
}
