//! A lock names the SDK from crates.io, as Cargo writes it for an author who
//! depends on it as on any crate. A lock naming it as a path package was made
//! with the SDK patched to a copy nobody uploaded, and is refused like any
//! other path package outside the archive, before it is paid for, telling
//! the author to take the SDK from crates.io and drop the patch.
//!
//! Sequence: author A uploads agents whose locks name the SDK as a path
//! package, at the carried version and at an earlier one. Expected: each is
//! refused with that message, and no job is made.

mod black_box;

use alloy::primitives::Address;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lock_taking_the_sdk_by_path_is_refused_before_it_is_paid_for() {
    let carried = strategy_sdk::VERSION;
    let parts: Vec<&str> = carried.split('.').collect();
    let earlier = format!("{}.{}.0", parts[0], parts[1]);

    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    for version in [carried, earlier.as_str()] {
        let lock = black_box::lock_with_sdk(&black_box::sdk_as_carried(version));
        let archive = black_box::agent_on_the_sdk(version, &lock);
        let (status, body) = builder.upload(Address::repeat_byte(0x7c), &archive).await;
        assert_eq!(
            status, 400,
            "a lock naming the SDK by path at {version} was taken: {body}"
        );
        assert!(
            body.contains(&format!(
                "{sdk} {version} as a path package: take {sdk} from crates.io",
                sdk = shared::dependencies::SDK
            )) && body.contains("drop any [patch] for it"),
            "a lock naming the SDK by path at {version} was refused as: {body}"
        );
    }
    assert!(builder.jobs().is_empty(), "a refused upload made a job");
}
