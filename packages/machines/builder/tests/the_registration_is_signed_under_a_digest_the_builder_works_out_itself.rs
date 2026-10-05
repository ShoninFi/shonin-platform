//! The builder's key signs a strategy's registration, and the registry
//! believes a registration signed so. The digest signed is worked out by the
//! builder from what it registers, never taken from the node: whoever
//! answers its RPC could otherwise hand it the digest of any registration,
//! or of any other message the key's signature counts for, and have it
//! signed.
//!
//! Sequence: the node answers every `registrationDigest` call with a digest
//! of its own choosing. The author uploads a project and pays; the reviewer
//! clears it; the stand-in `cargo` builds it. The job ends, and the key never
//! signed the node's digest.

// Setting PATH for the builder this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;
use std::time::Duration;

use alloy::primitives::{Address, B256};

/// A stand-in for `cargo build` that leaves a component where the builder
/// looks for one.
const FAKE_CARGO: &str = r#"#!/bin/sh
mkdir -p "$CARGO_TARGET_DIR/wasm32-wasip2/release"
printf '\000asm\015\000\001\000' > "$CARGO_TARGET_DIR/wasm32-wasip2/release/agent.wasm"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_registration_is_signed_under_a_digest_the_builder_works_out_itself() {
    let bin = black_box::scratch().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let cargo = bin.join("cargo");
    std::fs::write(&cargo, FAKE_CARGO).unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // SAFETY: set once, before anything else in this test binary runs.
    unsafe { std::env::set_var("PATH", path) };
    black_box::CLEARS.store(true, Ordering::SeqCst);

    let lie = B256::repeat_byte(0xd1);
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    *builder.node.lying_digest.lock().unwrap() = Some(lie);
    let author = Address::repeat_byte(0x77);
    let (status, body) = builder.upload(author, &black_box::project("signed")).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    builder
        .node
        .pay(0x11, 5, author, accepted.upload.parse().unwrap());
    builder.node.head.store(0x12, Ordering::SeqCst);

    let jobs = builder
        .wait_for(Duration::from_secs(30), |jobs| {
            jobs.iter()
                .any(|job| job.id == accepted.job_id && job.state.is_finished())
        })
        .await;
    let signed = builder.node.signed.lock().unwrap().clone();
    assert!(
        !signed.contains(&lie),
        "the builder's key signed the digest the node handed it, not one it worked out from \
         the registration (jobs {jobs:?})"
    );
    assert!(
        builder
            .said
            .lock()
            .unwrap()
            .contains("is not the one worked out here"),
        "a registry digest unlike the builder's own was not refused:\n{}",
        builder.said.lock().unwrap()
    );
}
