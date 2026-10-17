//! Mining stops for four seconds just as a slot's valuation is sent, so it
//! lands after the window and reverts: neither keeper sends it again in that
//! slot, and the candidate's sends are `main`'s. Anvil sets its interval in
//! whole seconds, so this runs at one and two-second blocks only. See
//! `differential/mod.rs`.

mod differential;

use differential::node::BlockTime;
use differential::settings::Profile;
use differential::{Fault, differs_no_more_than_the_baseline_with as check};

/// The slot at t0+20: mining stops at t0+21.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn at_two_second_blocks() {
    check(
        BlockTime::TwoSeconds,
        Profile::checked_in(),
        &[Fault::MiningPause { at: 21, pause: 4 }],
    )
    .await;
}

/// The slot at t0+20: mining stops at t0+21.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn at_one_second_blocks() {
    check(
        BlockTime::OneSecond,
        Profile::checked_in(),
        &[Fault::MiningPause { at: 21, pause: 4 }],
    )
    .await;
}
