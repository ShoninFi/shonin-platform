//! An upload is answered as taken only once the queue naming it is written
//! down. One answered 200 although `queue.json` could not be written would be
//! lost at the next restart (its archive swept, since no job on disk names
//! it), and the $1 paid against its reference would admit nothing. It is
//! turned away instead, its archive removed, and the author can send it
//! again.
//!
//! Sequence: `queue.json.new` can't be made (here a directory is in the way;
//! in production, a full disk or a read-only store). The author uploads and
//! is answered 503, and no archive is kept for it. Once the queue can be
//! written again, the same upload sent again is taken, and written down.

mod black_box;

use alloy::primitives::Address;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_that_could_not_be_written_down_is_turned_away() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let in_the_way = builder.store().join("queue.json.new");
    std::fs::create_dir_all(&in_the_way).unwrap();
    let author = Address::repeat_byte(0x77);
    let project = black_box::project("never written");

    let (status, body) = builder.upload(author, &project).await;
    assert_eq!(
        status,
        503,
        "an upload was answered as taken although it was never written down: {body}\n{}",
        builder.said.lock().unwrap()
    );
    let archives: Vec<_> = std::fs::read_dir(builder.store().join("archives"))
        .map(|found| found.flatten().map(|file| file.path()).collect())
        .unwrap_or_default();
    assert!(
        archives.is_empty(),
        "an upload turned away left its archive: {archives:?}"
    );

    std::fs::remove_dir(&in_the_way).unwrap();
    let (status, body) = builder.upload(author, &project).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    assert!(
        builder.jobs().iter().any(|job| job.id == accepted.job_id),
        "the upload sent again was answered as taken, yet is not written down"
    );
}
