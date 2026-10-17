//! At one-second blocks (Hyperliquid's local chain), the keeper sends every EVM
//! duty's transactions as `main`'s keeper does: the same calls, in the same
//! order, each in the same slot, and in blocks no further from `main`'s than
//! `main`'s own two runs are from each other. Each test is one RPC settings
//! profile of the candidate's; see `differential/mod.rs` for how a run is
//! made and `differential/world.rs` for the scenario.

mod differential;

use differential::differs_no_more_than_the_baseline_from_itself as check;
use differential::node::BlockTime::OneSecond as AT;
use differential::settings::{self, Profile};

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn at_the_checked_in_settings() {
    check(AT, Profile::checked_in()).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn at_the_fastest_active_round_and_notice() {
    check(AT, Profile::fastest()).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn at_the_longest_notice() {
    check(AT, Profile::slowest_notice()).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn at_the_least_head_lag_that_stays_bounded() {
    check(AT, Profile::least_head_lag(AT)).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn at_the_most_head_lag_accepted() {
    check(AT, Profile::most_head_lag(AT)).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn with_two_observations_exactly_filling_the_window() {
    check(AT, Profile::two_observations_exactly(AT)).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn with_the_head_lag_a_millisecond_inside_the_slot_lead() {
    check(AT, Profile::bounded_head_exactly(AT)).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn with_the_halt_grace_a_second_past_notice_and_an_ask() {
    let config = settings::checked_in_config();
    let notice =
        settings::checked_in_setting(&config, AT.chain_id(), "keeper_notice_ms").unwrap_or(2_000);
    check(AT, Profile::grace_after_notice_exactly(notice)).await;
}
