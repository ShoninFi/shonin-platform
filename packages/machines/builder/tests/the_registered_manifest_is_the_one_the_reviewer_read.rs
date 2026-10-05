//! The manifest the builder registers is the one uploaded and reviewed, read
//! from the archive itself, never read back from the build directory once
//! the build has run. A project's `build.rs` (or a procedural macro) runs
//! inside that build, with that directory as its working directory, so it
//! can rewrite `shonin.toml` there; what is registered (`manifestHash`, the
//! chains and drivers, the bridges, the restricted list) and stored as
//! `manifest.json` must not be that rewrite.
//!
//! Sequence: the author uploads a project whose `shonin.toml` says title
//! "An agent", no claims, and pays for it. The reviewer clears it. The build
//! runs, and a stand-in `cargo` on PATH does what a `build.rs` can: it
//! rewrites `shonin.toml` to claim "Never uses leverage" under another
//! title. The manifest stored is still the one uploaded.

// Setting PATH for the builder this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::Address;

/// A stand-in for `cargo build`: does what a project's `build.rs` may do
/// (rewrite the manifest beside it), then leaves a component where the
/// builder looks for one.
const FAKE_CARGO: &str = r#"#!/bin/sh
cat > shonin.toml <<'EOF'
[vault]
chain = "base"
[description]
title = "Rewritten after review"
text = "x"
claims = ["Never uses leverage"]
[drivers.local]
EOF
mkdir -p "$CARGO_TARGET_DIR/wasm32-wasip2/release"
printf '\000asm\015\000\001\000' > "$CARGO_TARGET_DIR/wasm32-wasip2/release/agent.wasm"
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_registered_manifest_is_the_one_the_reviewer_read() {
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

    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0x77);
    let (status, body) = builder.upload(author, &black_box::project("built")).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    builder
        .node
        .pay(0x11, 5, author, accepted.upload.parse().unwrap());
    builder.node.head.store(0x12, Ordering::SeqCst);

    // The manifest the builder stores is the one it registers.
    let started = Instant::now();
    let stored = loop {
        let found = std::fs::read_dir(builder.store())
            .unwrap()
            .flatten()
            .map(|entry| entry.path().join("manifest.json"))
            .find(|path| path.exists());
        if let Some(path) = found {
            break serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).unwrap())
                .unwrap();
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "nothing was built and stored: {:?}\n{}",
            builder.jobs(),
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let description = &stored["manifest"]["description"];
    assert_eq!(
        (description["title"].as_str(), description["claims"].clone()),
        (Some("An agent"), serde_json::json!([])),
        "the builder registers a manifest the build rewrote after the review, not the one \
         uploaded and reviewed: {stored:#}"
    );
}
