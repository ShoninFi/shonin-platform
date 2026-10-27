//! The public development mnemonic's Solana keys are public along every
//! common path, not only `m/44'/501'/<i>'/0'`: `solana-keygen recover` with
//! no derivation path takes the seed's first 32 bytes, `m/44'/501'/<i>'` is
//! the other common wallet path, and `m/44'/501'` an older one. Mainnet
//! refuses each of them as ADMIN, as it refuses the standard path.
//!
//! No node answers on the endpoint named: nothing reaches any cluster.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use alloy::signers::local::coins_bip39::{English, Mnemonic};
use chain::solana::{Keypair, Signer, keys};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn hmac_sha512(key: &[u8], message: &[u8]) -> [u8; 64] {
    use sha2::{Digest, Sha512};
    let mut block = [0u8; 128];
    block[..key.len()].copy_from_slice(key);
    let pad = |byte: u8| block.iter().map(|b| b ^ byte).collect::<Vec<u8>>();
    let inner = Sha512::new()
        .chain_update(pad(0x36))
        .chain_update(message)
        .finalize();
    Sha512::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

fn seed() -> Vec<u8> {
    Mnemonic::<English>::new_from_phrase(keys::DEVELOPMENT_MNEMONIC)
        .unwrap()
        .to_seed(None)
        .unwrap()
        .to_vec()
}

/// SLIP-0010 ed25519, every index hardened.
fn slip10(path: &[u32]) -> Keypair {
    let master = hmac_sha512(b"ed25519 seed", &seed());
    let (mut key, mut chain) = (master[..32].to_vec(), master[32..].to_vec());
    for step in path {
        let mut data = vec![0];
        data.extend_from_slice(&key);
        data.extend_from_slice(&(step | 0x8000_0000u32).to_be_bytes());
        let child = hmac_sha512(&chain, &data);
        (key, chain) = (child[..32].to_vec(), child[32..].to_vec());
    }
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&key);
    Keypair::new_from_array(secret)
}

fn dry_run_admin_steps(admin: &str) -> Output {
    let dir = std::env::temp_dir().join(format!(
        "solana-public-admin-{}-{}",
        &admin[..8],
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("deployments")).unwrap();
    std::fs::write(
        dir.join("deployments/solana-mainnet-beta.json"),
        r#"{"programs": {}}"#,
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_devnet"))
        .current_dir(repo())
        .env_remove("SHONIN_NETWORK")
        .env_remove("SHONIN_CHAINS")
        .env("SHONIN_RPC_SOLANA_MAINNET_BETA", "http://127.0.0.1:1")
        .args(["--store", dir.join("store").to_str().unwrap()])
        .args(["--artifacts", dir.join("artifacts").to_str().unwrap()])
        .args(["--network", "mainnet", "--vault-chain", "solana"])
        .args([
            "--deployments-dir",
            dir.join("deployments").to_str().unwrap(),
        ])
        .args(["mainnet-solana-admin-steps", "--admin", admin])
        .args(["--admin-steps-dir", dir.join("steps").to_str().unwrap()])
        .output()
        .expect("the devnet binary");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn said(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn refused_as_public(admin: &Keypair, path: &str) {
    let out = dry_run_admin_steps(&admin.pubkey().to_string());
    let text = said(&out);
    assert!(
        !out.status.success() && text.contains("public development mnemonic"),
        "ADMIN={} (the public development mnemonic at {path}) was not refused as public:\n{text}",
        admin.pubkey()
    );
}

#[test]
fn the_standard_path_s_key_is_refused_as_public() {
    let standard = keys::keypair_from_mnemonic(keys::DEVELOPMENT_MNEMONIC, 0).unwrap();
    assert_eq!(slip10(&[44, 501, 0, 0]).pubkey(), standard.pubkey());
    refused_as_public(&standard, "m/44'/501'/0'/0'");
}

#[test]
fn solana_keygen_s_default_key_of_the_public_mnemonic_is_refused_as_public() {
    // `solana-keygen recover` with no derivation path: the seed's first 32
    // bytes are the ed25519 secret.
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&seed()[..32]);
    refused_as_public(&Keypair::new_from_array(secret), "no derivation path");
}

#[test]
fn the_three_level_wallet_path_s_key_of_the_public_mnemonic_is_refused_as_public() {
    refused_as_public(&slip10(&[44, 501, 0]), "m/44'/501'/0'");
    refused_as_public(&slip10(&[44, 501, 999]), "m/44'/501'/999'");
}

#[test]
fn the_two_level_wallet_path_s_key_of_the_public_mnemonic_is_refused_as_public() {
    refused_as_public(&slip10(&[44, 501]), "m/44'/501'");
}

#[test]
fn the_standard_path_s_last_refused_index_is_refused_as_public() {
    let last = keys::keypair_from_mnemonic(keys::DEVELOPMENT_MNEMONIC, 999).unwrap();
    assert_eq!(slip10(&[44, 501, 999, 0]).pubkey(), last.pubkey());
    refused_as_public(&last, "m/44'/501'/999'/0'");
}
