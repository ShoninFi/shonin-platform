//! Every chain's clock runs behind the wall, so each head the keepers read is
//! older than usual: a second (inside one-second chains' `head_lag_max_ms`, so
//! the candidate keeps to its wakes) or four (outside it, so the candidate
//! reads on its active path). Either way it sends what `main` sends, as
//! `main` sends it. See `differential/mod.rs`.

mod differential;

use differential::node::BlockTime;
use differential::settings::Profile;
use differential::{Fault, differs_no_more_than_the_baseline_with as check};

/// A second behind: inside the bound.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn a_head_a_second_old_inside_the_lag_bound() {
    check(
        BlockTime::OneSecond,
        Profile::checked_in(),
        &[Fault::HeadsBehind(1)],
    )
    .await;
}

/// Four seconds behind: outside the bound.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn a_head_four_seconds_old_outside_the_lag_bound() {
    check(
        BlockTime::OneSecond,
        Profile::checked_in(),
        &[Fault::HeadsBehind(4)],
    )
    .await;
}
