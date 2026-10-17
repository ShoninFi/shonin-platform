//! One station's `value()` costs more gas than one read of the candidate's
//! batched observation may use: the candidate reads it alone, and sends what
//! `main` sends. See `differential/mod.rs`.

mod differential;

use differential::node::BlockTime;
use differential::settings::Profile;
use differential::{Fault, differs_no_more_than_the_baseline_with as check};

/// The units vault's station.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn a_station_value_of_one_and_a_half_million_gas() {
    check(
        BlockTime::TwoSeconds,
        Profile::checked_in(),
        &[Fault::CostlyView],
    )
    .await;
}
