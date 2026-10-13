//! A manifest's requirement on the SDK is one the carried SDK must satisfy:
//! the build service builds against its own copy, so an agent requiring
//! another version (`=0.1.0`, `<0.1.1`) would be paid for and then fail to
//! resolve. It is refused when it is uploaded, before it is paid for, telling
//! the author which version to depend on.
//!
//! Sequence: author A uploads agents whose lock takes the SDK from crates.io
//! at the carried version and whose manifests require an earlier one.
//! Expected: each is refused, naming the requirement, and no job is made.

mod black_box;

use alloy::primitives::Address;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_sdk_requirement_the_carried_copy_does_not_satisfy_is_refused_before_it_is_paid_for() {
    let carried = strategy_sdk::VERSION;
    let parts: Vec<&str> = carried.split('.').collect();
    let earlier = format!("{}.{}.0", parts[0], parts[1]);
    assert_ne!(earlier, carried);

    let sdk = shared::dependencies::SDK;
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    for requirement in [format!("={earlier}"), format!("<{carried}")] {
        let archive = black_box::agent_on_the_sdk(
            &requirement,
            &black_box::lock_with_sdk(&black_box::sdk_from_crates_io(carried, &"5".repeat(64))),
        );
        let (status, body) = builder.upload(Address::repeat_byte(0x79), &archive).await;
        assert_eq!(
            status, 400,
            "an agent requiring {sdk} = \"{requirement}\", which the carried {carried} does not \
             satisfy, was taken: cargo --locked would fail after payment\n{body}"
        );
        assert!(
            body.contains(&format!("requires {sdk} = \"{requirement}\""))
                && body.contains(&format!("depend on {sdk} = \"{carried}\"")),
            "{requirement} was refused as: {body}"
        );
    }
    assert!(builder.jobs().is_empty(), "a refused upload made a job");
}
