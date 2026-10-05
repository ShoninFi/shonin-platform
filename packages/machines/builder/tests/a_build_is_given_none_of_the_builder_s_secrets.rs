//! A strategy's build runs the author's code: its build scripts and
//! procedural macros, and `env!` / `option_env!` read at compile time. On a
//! testnet the build service's environment carries
//! `SHONIN_TESTNET_EVM_MNEMONIC`, from which every role account is derived
//! (the admin, the attestor that stands in for the attestation hardware, the
//! builder and the reviewer), and any RPC override (`SHONIN_RPC_*`, which may
//! carry a provider's API key). Handed to a build, a reviewed-looking
//! `option_env!(...)` would bake them into the component, which the builder
//! writes to the public store.
//!
//! So a build is given none of the build service's environment, only what
//! compiling needs. Here the build service runs with a testnet mnemonic and
//! an RPC override in its environment; an author uploads a project and pays;
//! the reviewer clears it; the stand-in `cargo` records the environment it
//! was run with, which holds neither.

// Setting PATH and the secrets for the builder this test starts is the only
// unsafe here.
#![allow(unsafe_code)]

mod black_box;

use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use alloy::primitives::Address;

const MNEMONIC: &str = "secret words that must never reach a strategy build";
const RPC: &str = "https://base.example.invalid/v2/provider-api-key-0123456789";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_build_is_given_none_of_the_builder_s_secrets() {
    let bin = black_box::scratch().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let seen = bin.join("environment");
    let cargo = bin.join("cargo");
    std::fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nenv > {}.part\nmv {0}.part {0}\nexit 1\n",
            seen.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // SAFETY: set once, before anything else in this test binary runs; the
    // builder this test starts inherits them, as a deployed one is given them.
    unsafe {
        std::env::set_var("PATH", path);
        std::env::set_var("SHONIN_TESTNET_EVM_MNEMONIC", MNEMONIC);
        std::env::set_var("SHONIN_RPC_BASE_SEPOLIA", RPC);
    }
    black_box::CLEARS.store(true, Ordering::SeqCst);

    let builder = black_box::start(black_box::node(0x10, None), &[]).await;
    let author = Address::repeat_byte(0x78);
    let (status, body) = builder.upload(author, &black_box::project("secrets")).await;
    assert_eq!(status, 200, "{body}");
    let accepted: shared::UploadAccepted = serde_json::from_str(&body).unwrap();
    builder
        .node
        .pay(0x11, 5, author, accepted.upload.parse().unwrap());
    builder.node.head.store(0x12, Ordering::SeqCst);

    let started = Instant::now();
    while !seen.exists() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the build never started:\n{}",
            builder.said.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let environment = std::fs::read_to_string(&seen).unwrap();
    let leaked: Vec<&str> = [MNEMONIC, RPC]
        .into_iter()
        .filter(|secret| environment.contains(secret))
        .collect();
    assert!(
        leaked.is_empty(),
        "the build (the author's build scripts, macros and option_env!) was handed the build \
         service's secrets: {leaked:?}"
    );
    // It is given what compiling needs.
    for needed in [
        "PATH=",
        "HOME=",
        "CARGO_HOME=",
        "CARGO_TARGET_DIR=",
        "RUSTUP_TOOLCHAIN=",
    ] {
        assert!(
            environment.lines().any(|line| line.starts_with(needed)),
            "the build was not given {needed}:\n{environment}"
        );
    }
}
