//! Builds the guest for the zkVM inside RISC Zero's guest-builder image in
//! Docker, so its image id is the same on every machine: the id a checker
//! pins can be rebuilt from source by anyone, as the executor's image can.
//!
//! Building it needs Docker and RISC Zero's toolchain, so it is opt-in:
//! `SHONIN_ATTESTATION_BUILD_GUEST=1` builds it. Without it, the crate holds
//! an empty ELF and a zero image id, which is all a build that never proves
//! (every ordinary build, and CI) needs; the image id every checker pins is
//! written in the deploy crate, and a test run with the guest built checks
//! the two agree.

use std::collections::HashMap;

use risc0_build::{DockerOptionsBuilder, GuestOptionsBuilder};

fn main() {
    println!("cargo:rerun-if-env-changed=SHONIN_ATTESTATION_BUILD_GUEST");
    if std::env::var_os("SHONIN_ATTESTATION_BUILD_GUEST").is_none() {
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
        std::fs::write(
            out.join("methods.rs"),
            "pub const SHONIN_ATTESTATION_GUEST_ELF: &[u8] = &[];\n\
             pub const SHONIN_ATTESTATION_GUEST_ID: [u32; 8] = [0; 8];\n\
             pub const SHONIN_ATTESTATION_GUEST_PATH: &str = \"\";\n",
        )
        .expect("writing the empty guest");
        return;
    }
    // The build context is the attestation directory, so the guest's path
    // dependency on the core crate is inside it.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let docker = DockerOptionsBuilder::default()
        .root_dir(root)
        // Pinned: the builder image is part of what the image id depends on.
        // Rust 1.91 (the guest's dependencies need at least 1.90), and an
        // image published for one platform, which Docker runs anywhere.
        .docker_container_tag("r0.1.91.1")
        .build()
        .expect("docker options");
    let guest = GuestOptionsBuilder::default()
        .use_docker(docker)
        .build()
        .expect("guest options");
    risc0_build::embed_methods_with_options(HashMap::from([("shonin-attestation-guest", guest)]));
}
