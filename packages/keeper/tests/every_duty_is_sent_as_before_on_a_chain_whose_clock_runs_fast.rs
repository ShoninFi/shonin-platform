//! On a chain whose clock moves an hour a block, no chain clock bound holds:
//! the candidate stays on its active path all run and sends what `main`
//! sends. See `differential/mod.rs`.

mod differential;

use differential::node::BlockTime;
use differential::settings::Profile;
use differential::{Fault, differs_no_more_than_the_baseline_with as check};

/// An hour of chain time a block.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs main's keeper binary in SHONIN_BASELINE_KEEPER"]
async fn an_hour_a_block() {
    check(
        BlockTime::OneSecond,
        Profile::checked_in(),
        &[Fault::FastClock(3_600)],
    )
    .await;
}
