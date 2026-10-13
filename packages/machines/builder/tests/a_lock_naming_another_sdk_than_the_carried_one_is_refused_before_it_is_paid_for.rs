//! The build service compiles every agent against the SDK it carries, and
//! a lock names the SDK from crates.io at exactly that version. A lock
//! naming any other version, earlier or later, was written against an SDK
//! the agent would not be built with, so it is refused when it is uploaded,
//! before it is paid for, telling the author which version to take.
//!
//! Sequence: author A uploads agents whose locks take the SDK from crates.io
//! at an earlier version, the next patch, the next minor and the next major
//! version past the carried one, and a pre-release of the carried one.
//! Expected: each is refused, naming the version it locks and the carried
//! one, and no job is made.

mod black_box;

use alloy::primitives::Address;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lock_naming_another_sdk_than_the_carried_one_is_refused_before_it_is_paid_for() {
    let sdk = shared::dependencies::SDK;
    let carried = strategy_sdk::VERSION;
    let parts: Vec<u64> = carried
        .split('.')
        .map(|part| part.parse().unwrap())
        .collect();
    let (major, minor, patch) = (parts[0], parts[1], parts[2]);
    let refused = [
        format!("{major}.{minor}.{}", patch.saturating_sub(1)),
        format!("{major}.{minor}.{}", patch + 1),
        format!("{major}.{}.0", minor + 1),
        format!("{}.0.0", major + 1),
        format!("{carried}-rc.1"),
    ];

    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    for version in refused.iter().filter(|version| *version != carried) {
        let archive = black_box::agent_on_the_sdk(
            carried,
            &black_box::lock_with_sdk(&black_box::sdk_from_crates_io(version, &"7".repeat(64))),
        );
        let (status, body) = builder.upload(Address::repeat_byte(0x7a), &archive).await;
        assert_eq!(
            status, 400,
            "a lock naming the SDK at {version} was taken, though the build service carries \
             {carried}: {body}"
        );
        assert!(
            body.contains(&format!("locks {sdk} {version} from crates.io"))
                && body.contains(&format!(
                    "take {sdk} = \"{carried}\" from crates.io and run `cargo update -p {sdk}`"
                )),
            "the SDK at {version} was refused without saying what to take: {body}"
        );
    }
    assert!(builder.jobs().is_empty(), "a refused upload made a job");
}
