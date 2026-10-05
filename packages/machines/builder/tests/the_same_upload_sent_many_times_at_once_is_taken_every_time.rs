//! The same upload sent many times at once (a double click, or a client
//! retrying while its first request is still being unpacked) is taken every
//! time, as the one upload, and leaves nothing unpacked behind.
//!
//! Each request unpacks into a directory of its own, so no request's files
//! land in the middle of another's.
//!
//! Sequence: one project, sent eight at a time, five times over. All 40 are
//! taken, as one job, and the builds directory is empty afterwards.

mod black_box;

use alloy::primitives::Address;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_same_upload_sent_many_times_at_once_is_taken_every_time() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0xa3);
    let archive = black_box::project("twice");
    let mut refused = Vec::new();
    for _round in 0..5 {
        let sends: Vec<_> = (0..8).map(|_| builder.upload(author, &archive)).collect();
        for (status, body) in black_box::all_at_once(sends).await {
            if status != 200 {
                refused.push((status, body));
            }
        }
    }
    assert!(
        refused.is_empty(),
        "{} of 40 copies of one good upload, sent eight at a time, were refused: {:?}",
        refused.len(),
        refused.iter().take(3).collect::<Vec<_>>()
    );
    assert_eq!(builder.jobs().len(), 1);
    let left: Vec<_> = std::fs::read_dir(builder.builds())
        .map(|found| found.flatten().map(|entry| entry.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "uploads left {left:?} unpacked");
}
