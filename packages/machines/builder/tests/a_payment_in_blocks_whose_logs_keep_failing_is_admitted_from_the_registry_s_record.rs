//! A payment in blocks whose logs keep failing is admitted from the
//! registry's record of it. An endpoint can fail one block's logs for good
//! (the logs pruned, a node that lost them): read again and again, the page
//! holding that block would stop every payment after it, and keep the build
//! service up for nothing. After a few tries the page is passed over, but
//! only with every payment the registry records in it: the registry numbers
//! its payments in order, its count before and through the page says which
//! they are, and each one's record gives its wallet and upload.
//!
//! Sequence: the chain stands at 0x40. An author pays at 0x20 for an upload
//! waiting since an hour ago, and no log of block 0x20 can be read. The
//! job is admitted all the same, and the builder says it passed over blocks
//! whose logs could not be read.

mod black_box;

use std::time::Duration;

use alloy::primitives::{Address, B256};
use shared::model::registry::JobState;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_payment_in_blocks_whose_logs_keep_failing_is_admitted_from_the_registry_s_record() {
    let author = Address::repeat_byte(0x77);
    let reference = B256::repeat_byte(0x44);
    let archive = black_box::project("paid");
    let queue = black_box::queue(vec![black_box::waiting(
        "8800",
        author,
        reference,
        &archive,
        time::Duration::hours(1),
    )]);
    let node = black_box::node(0x40, None);
    node.pay(0x20, 1, author, reference);
    node.unreadable.lock().unwrap().push(0x20);
    let builder = black_box::start(node, &[queue]).await;
    let jobs = builder
        .wait_for(Duration::from_secs(40), |jobs| {
            jobs.iter().any(|job| job.strategy_id == Some(1))
        })
        .await;
    let said = builder.said.lock().unwrap().clone();
    assert!(
        jobs.iter().any(|job| job.id == "8800"
            && job.strategy_id == Some(1)
            && job.state != JobState::AwaitingPayment),
        "a payment in a block whose logs can't be read was never admitted: {jobs:?}\n{said}"
    );
    assert!(
        said.contains("passed over blocks whose logs could not be read"),
        "{said}"
    );
}
