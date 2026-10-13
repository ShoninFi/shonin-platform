//! The build service compiles every agent against the SDK it carries. A
//! lock naming a newer SDK, or one the carried SDK is not compatible with,
//! was written against an SDK the agent would not be built with, so it is
//! refused when it is uploaded, before it is paid for, with a message naming
//! the version the build service carries.
//!
//! Sequence: author A uploads agents whose locks take the SDK from crates.io
//! at the next patch, the next minor and the next major version past the
//! carried one, and at an earlier version it is not compatible with.
//! Expected: each is refused, naming the version it locks and the carried
//! one, and no job is made.

mod black_box;

use alloy::primitives::Address;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newer_or_incompatible_sdk_is_refused_before_it_is_paid_for() {
    let carried = strategy_sdk::VERSION;
    let parts: Vec<u64> = carried
        .split('.')
        .map(|part| part.parse().unwrap())
        .collect();
    let (major, minor, patch) = (parts[0], parts[1], parts[2]);
    let refused = [
        format!("{major}.{minor}.{}", patch + 1),
        format!("{major}.{}.0", minor + 1),
        format!("{}.0.0", major + 1),
        // Before 1.0, a version is compatible only within its minor.
        "0.0.9".to_string(),
    ];

    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    for version in &refused {
        let archive = black_box::agent_on_the_sdk(
            version,
            &black_box::lock_with_sdk(&black_box::sdk_from_crates_io(version, &"7".repeat(64))),
        );
        let (status, body) = builder.upload(Address::repeat_byte(0x7a), &archive).await;
        assert_eq!(
            status, 400,
            "a lock naming the SDK at {version} was taken, though the build service carries \
             {carried}: {body}"
        );
        assert!(
            body.contains(&format!("{} {version}", shared::dependencies::SDK))
                && body.contains(&format!(
                    "carries and builds every agent against, {carried}"
                )),
            "the SDK at {version} was refused without naming the version carried: {body}"
        );
    }
    assert!(builder.jobs().is_empty(), "a refused upload made a job");
}
