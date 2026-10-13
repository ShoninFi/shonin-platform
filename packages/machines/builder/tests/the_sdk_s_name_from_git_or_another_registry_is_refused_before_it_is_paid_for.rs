//! The SDK may be named from crates.io because the build service builds
//! against its own copy whatever the lock says. Nothing else rides on that:
//! a crate under the SDK's name from git or from another registry is code
//! nobody reviewed, and is refused as any such crate is.
//!
//! Sequence: author A uploads agents whose locks take a crate named as the
//! SDK from git, from another registry, and from another registry's sparse
//! index. Expected: each is refused before it is paid for, and no job is
//! made.

mod black_box;

use alloy::primitives::Address;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sdk_s_name_from_git_or_another_registry_is_refused_before_it_is_paid_for() {
    let carried = strategy_sdk::VERSION;
    let git = "0123456789abcdef0123456789abcdef01234567";
    let crates_io = format!("source = \"{}\"", shared::dependencies::CRATES_IO);
    let sdk = black_box::sdk_from_crates_io(carried, &"8".repeat(64));
    let refused = [
        (
            format!("source = \"git+https://example.invalid/sdk.git?rev={git}#{git}\""),
            "git",
        ),
        (
            "source = \"registry+https://example.invalid/index\"".to_string(),
            "a registry other than crates.io",
        ),
        (
            "source = \"sparse+https://example.invalid/index/\"".to_string(),
            "a registry other than crates.io",
        ),
    ];

    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    for (source, why) in &refused {
        let lock = black_box::lock_with_sdk(&sdk.replace(&crates_io, source));
        let archive = black_box::agent_on_the_sdk(carried, &lock);
        let (status, body) = builder.upload(Address::repeat_byte(0x7b), &archive).await;
        assert_eq!(
            status, 400,
            "a crate named as the SDK, locked with {source}, was taken: {body}"
        );
        assert!(
            body.contains(why),
            "a crate named as the SDK, locked with {source}, was refused as: {body}"
        );
    }
    assert!(builder.jobs().is_empty(), "a refused upload made a job");
}
