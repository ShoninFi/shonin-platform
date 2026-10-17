//! A third holder takes units at t0+50, its `ProRataTaken` emitted then on
//! every chain by one identical harness transaction. The candidate's node
//! leaves that log out of its answers, and fails every header read by
//! number, from t0+49 until t0+70, long after the candidate's first looks
//! have caught its log reader up: the candidate serves the holder only once
//! its log can be read and checked, and then once; `main`, whose node
//! withholds nothing, serves it at once. See `differential/mod.rs`.

mod differential;

use differential::node::BlockTime;
use differential::settings::Profile;
use differential::{Fault, differs_no_more_than_the_baseline_with as check};

/// The log lands at t0+50 and is withheld until t0+70.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn withheld_from_the_moment_it_lands_for_twenty_seconds() {
    check(
        BlockTime::TwoSeconds,
        Profile::checked_in(),
        &[Fault::LogWithheld { at: 50, until: 70 }],
    )
    .await;
}
