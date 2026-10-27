//! Every key of anvil's public development mnemonic is public, not only the
//! ten an anvil lists by default: `anvil --accounts 20` (and Hardhat's
//! default node) prints the eleventh and on, keys and all. A real deploy
//! refuses any of them as ADMIN, as it refuses the first ten.
//!
//! No node answers on the port the chains file names, and a real send
//! refuses a loopback endpoint: nothing reaches any chain whatever the
//! outcome.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use alloy::signers::local::PrivateKeySigner;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("shonin-public-admin-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn devnet(dir: &Path, args: &[&str]) -> Output {
    let chains = dir.join("chains.toml");
    std::fs::write(
        &chains,
        "[chains.\"evm:8453\"]\nlabel = \"Base\"\nrpc = \"http://127.0.0.1:1\"\n\
         dollar = \"USDC\"\n\n[networks.mainnet]\nbase = \"evm:8453\"\nroot = \"base\"\n",
    )
    .unwrap();
    Command::new(env!("CARGO_BIN_EXE_devnet"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .current_dir(repo())
        .args(["--network", "mainnet", "--vault-chain", "base"])
        .arg("--chains")
        .arg(&chains)
        .arg("--deployments-dir")
        .arg(dir.join("deployments"))
        .arg("--store")
        .arg(dir.join("store"))
        .arg("--artifacts")
        .arg(repo().join("contracts/evm/out"))
        .args(args)
        .output()
        .expect("the devnet binary")
}

fn said(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The refusal names ADMIN as the reason, not the endpoint the run would
/// next have refused.
fn refused_for_the_admin(output: &Output, why: &[&str]) {
    let text = said(output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.contains("Nothing was sent"), "{text}");
    let first = text
        .lines()
        .find(|line| line.contains("Error") || line.contains("refused"))
        .unwrap_or(&text)
        .to_string();
    assert!(
        first.contains("ADMIN") && why.iter().any(|word| first.contains(word)),
        "a real deploy went past ADMIN={} (refused only later, for: {first})",
        why.join("/")
    );
}

#[test]
fn a_real_deploy_refuses_any_key_of_the_public_development_mnemonic_as_admin() {
    let dir = scratch("eleventh");
    // The development mnemonic's eleventh account (index 10): Hardhat's
    // default node and `anvil --accounts 20` print this key.
    let key = dir.join("admin-key");
    std::fs::write(
        &key,
        "0xf214f2b2cd398c806f84e317254e0f0b801d0643303237d97a22a48e01628897\n",
    )
    .unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let deployer = PrivateKeySigner::random();
    let file = dir.join("deployer");
    std::fs::write(
        &file,
        format!("0x{}\n", alloy::hex::encode(deployer.to_bytes())),
    )
    .unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let output = devnet(
        &dir,
        &[
            "mainnet-deploy",
            "--admin",
            "0xBcd4042DE499D14e55001CcbB24a551F3b954096",
            "--admin-keyfile",
            key.to_str().unwrap(),
            "--deployer-keyfile",
            file.to_str().unwrap(),
            "--confirm-mainnet",
            "8453",
            "--mainnet-generation",
            "1",
            "--admin-steps-dir",
            dir.join("steps").to_str().unwrap(),
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    refused_for_the_admin(&output, &["anvil", "public"]);
}
