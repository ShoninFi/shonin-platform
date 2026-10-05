//! A `shonin.toml` beside a `Cargo.toml` that has no `[package]` (a pure
//! workspace root) has no package to build: it is refused when it is
//! uploaded, before anyone pays, saying where the manifest belongs.
//!
//! Sequence: a project whose only `shonin.toml` sits beside a workspace's
//! root `Cargo.toml`, the agent a member under `agent/`, is uploaded. It is
//! refused with a 400 naming the missing `[package]`, and nothing is queued.

mod black_box;

use alloy::primitives::Address;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manifest_beside_a_workspace_root_is_refused_before_it_is_paid_for() {
    let root = black_box::scratch().join("project");
    std::fs::create_dir_all(root.join("agent/src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"agent\"]\n\n[workspace.package]\nname = \"agent\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("shonin.toml"),
        "[vault]\nchain = \"base\"\n[description]\ntitle = \"An agent\"\ntext = \"x\"\n\
         [drivers.local]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("agent/Cargo.toml"),
        "[package]\nname = \"agent\"\n",
    )
    .unwrap();
    std::fs::write(root.join("agent/src/lib.rs"), "// the agent\n").unwrap();
    let archive = shared::archive::pack(&root).unwrap();

    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let (status, body) = builder.upload(Address::repeat_byte(0x78), &archive).await;
    assert_eq!(
        status, 400,
        "a manifest beside a workspace's root was taken: {body}"
    );
    assert!(body.contains("[package]"), "{body}");
    assert!(builder.jobs().is_empty());
}
