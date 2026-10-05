//! A project within the source limits spread over many files is taken: the
//! archive carries a 512-byte tar header and up to 511 bytes of padding per
//! file besides its source, and the upload's body limit leaves room for them.
//! A body past that limit is turned away in terms of what a project may
//! hold, not axum's "length limit exceeded".
//!
//! Sequence: an author uploads a project with 600,000 bytes of source in
//! 1,000 files of 600 bytes, within what the reviewer reads in one review:
//! about 1.5 MB archived, 2 MB as base64. Then a body just past the upload
//! limit.

mod black_box;

use alloy::primitives::Address;

fn project_in_many_files(files: usize, each: usize) -> Vec<u8> {
    let root = black_box::scratch().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"agent\"\n").unwrap();
    std::fs::write(root.join("Cargo.lock"), black_box::lock(&["agent"], &[])).unwrap();
    std::fs::write(
        root.join("shonin.toml"),
        "[vault]\nchain = \"base\"\n[description]\ntitle = \"An agent\"\ntext = \"x\"\n\
         [drivers.local]\n",
    )
    .unwrap();
    for n in 0..files {
        std::fs::write(root.join(format!("src/m{n:04}.rs")), "/".repeat(each)).unwrap();
    }
    let packed = shared::archive::pack(&root).unwrap();
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
    packed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_source_within_the_limit_in_many_files_is_taken() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let archive = project_in_many_files(1_000, 600);
    let (status, body) = builder.upload(Address::repeat_byte(0x77), &archive).await;
    assert_eq!(
        status,
        200,
        "a project with 600,000 bytes of source (limit {}) in 1,000 files was turned away \
         ({} bytes archived): {body}",
        shared::archive::MOST_SOURCE_BYTES,
        archive.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_past_the_body_limit_is_refused_for_what_a_project_may_hold() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let archive = vec![0u8; shared::archive::MOST_UPLOAD_BYTES * 3 / 4 + 1];
    let (status, body) = builder.upload(Address::repeat_byte(0x77), &archive).await;
    assert_eq!(status, 413, "{body}");
    assert!(
        body.contains("MiB of source") && body.contains("files"),
        "the refusal does not say what a project may hold: {body}"
    );
}
