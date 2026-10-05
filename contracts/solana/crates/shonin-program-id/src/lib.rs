//! A program's address, decided at build time so that no local or devnet
//! address is ever committed. Every program's `build.rs` calls [`emit`].
//!
//! With the `mainnet` feature it is the committed id in `mainnet-id`, next to
//! the program's `Cargo.toml`. Otherwise it is the address of the keypair
//! Anchor deploys the program with, `target/deploy/<name>-keypair.json`, which
//! never leaves the machine (or, for devnet, is derived from the team's
//! mnemonic). Without that keypair, as on a fresh checkout's `cargo check`, a
//! placeholder stands in.

use std::path::PathBuf;

/// The system program's address: a valid key, and obviously not ours.
const PLACEHOLDER: &str = "11111111111111111111111111111111";

/// Writes `program_id.rs` into `OUT_DIR` for the program whose library is
/// named `lib_name` (as in `[lib] name`), for `include!` in its `lib.rs`.
pub fn emit(lib_name: &str) {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let id = if std::env::var_os("CARGO_FEATURE_MAINNET").is_some() {
        let path = manifest.join("mainnet-id");
        println!("cargo:rerun-if-changed={}", path.display());
        let id = std::fs::read_to_string(&path).unwrap_or_default();
        let id = id.trim();
        assert!(
            !id.is_empty(),
            "no mainnet id is committed yet: it goes in {}",
            path.display()
        );
        id.to_string()
    } else {
        let keypair = manifest.join(format!("../../target/deploy/{lib_name}-keypair.json"));
        println!("cargo:rerun-if-changed={}", keypair.display());
        match std::fs::read_to_string(&keypair) {
            Ok(text) => address_of(&text),
            Err(_) => PLACEHOLDER.to_string(),
        }
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets it"));
    std::fs::write(
        out.join("program_id.rs"),
        format!("anchor_lang::declare_id!(\"{id}\");\n"),
    )
    .expect("writing the program id");
}

/// A keypair file is the secret then the public key, as 64 numbers.
fn address_of(text: &str) -> String {
    let bytes: Vec<u8> = text
        .trim()
        .trim_matches(['[', ']'])
        .split(',')
        .map(|n| n.trim().parse().expect("a keypair file holds numbers"))
        .collect();
    assert_eq!(bytes.len(), 64, "a keypair file holds 64 bytes");
    bs58::encode(&bytes[32..]).into_string()
}
