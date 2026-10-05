//! An upload whose source is within the source limits is taken, not turned
//! away by the size of the request carrying it: the archive travels as
//! base64 (4/3 its size, and a tar header for each file) inside JSON, past
//! axum's default 2 MiB body limit. One over the source limit (2 MiB) is
//! still refused by the build service's own check, which says why.

mod black_box;

use alloy::primitives::Address;

/// A project like [`black_box::project`], carrying `size` bytes of source
/// beside it, in `files` files.
fn project_of(files: usize, size: usize) -> Vec<u8> {
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
    let line = "// a line of the agent's own source, long enough to fill it quickly\n";
    let body = line.repeat(size / files / line.len());
    for n in 0..files {
        std::fs::write(root.join(format!("src/m{n:04}.rs")), &body).unwrap();
    }
    let packed = shared::archive::pack(&root).unwrap();
    let _ = std::fs::remove_dir_all(root.parent().unwrap());
    packed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_within_the_source_limit_is_taken() {
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0x77);

    // Within what the reviewer reads in one review, yet in enough files
    // that the body is past axum's default.
    let within = project_of(2_000, 600_000);
    assert!(shared::base64::encode(&within).len() > 2 * 1024 * 1024);
    let (status, body) = builder.upload(author, &within).await;
    assert_eq!(
        status, 200,
        "a project within the source limit was turned away: {body}"
    );

    let (status, body) = builder.upload(author, &project_of(1, 2_200_000)).await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains(shared::Failure::TooLarge.label()), "{body}");
}
