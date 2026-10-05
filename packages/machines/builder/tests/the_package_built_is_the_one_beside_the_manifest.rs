//! The builder registers the `shonin.toml` it found and builds the package
//! beside it: `[package].name` in the `Cargo.toml` there, whatever other
//! tables name. A `Cargo.toml` may write its `[lib]` table above `[package]`
//! (TOML doesn't care about table order, and neither does cargo); building
//! `-p <the lib's name>` would build, in a workspace with a member of that
//! name, a different crate from the one whose manifest is registered, and
//! which the reviewer was never told is the agent.
//!
//! Sequence: author A uploads a project whose `Cargo.toml` beside
//! `shonin.toml` reads `[lib] name = "helper"` then `[package] name =
//! "agent"`; pays; the reviewer clears it. The stand-in `cargo` records the
//! arguments it was run with: `-p agent`.

// Setting PATH for the builder this test starts is the only unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;
use std::time::Duration;

use alloy::primitives::Address;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_package_built_is_the_one_beside_the_manifest() {
    let bin = black_box::scratch().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let said = bin.join("cargo-args");
    let cargo = bin.join("cargo");
    std::fs::write(
        &cargo,
        format!("#!/bin/sh\necho \"$@\" > {}\nexit 1\n", said.display()),
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // SAFETY: set once, before anything else in this test binary runs.
    unsafe { std::env::set_var("PATH", path) };
    black_box::CLEARS.store(true, Ordering::SeqCst);

    let root = black_box::scratch().join("project");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[lib]\nname = \"helper\"\ncrate-type = [\"cdylib\"]\n\n[package]\nname = \"agent\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("shonin.toml"),
        "[vault]\nchain = \"base\"\n[description]\ntitle = \"An agent\"\ntext = \"x\"\n\
         [drivers.local]\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), "// the agent\n").unwrap();
    std::fs::write(root.join("Cargo.lock"), black_box::lock(&["agent"], &[])).unwrap();
    let archive = shared::archive::pack(&root).unwrap();

    let author = Address::repeat_byte(0x77);
    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let (status, body) = builder.upload(author, &archive).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    builder
        .node
        .pay(0x11, 5, author, accepted.upload.parse().unwrap());
    builder.node.head.store(0x12, Ordering::SeqCst);

    builder
        .wait_for(Duration::from_secs(30), |jobs| {
            jobs.iter()
                .any(|job| job.id == accepted.job_id && job.state.is_finished())
        })
        .await;
    let args = std::fs::read_to_string(&said).unwrap_or_default();
    assert!(
        args.split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| pair == ["-p", "agent"]),
        "the builder built another package than the one beside the shonin.toml it registers: \
         cargo was run with `{}`",
        args.trim()
    );
}
