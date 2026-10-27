//! A mainnet deploy makes ADMIN the admin of everything for good, so every
//! mainnet mode but a fork rehearsal (the real deploy, its dry run, the
//! read-back) refuses an ADMIN anyone holds (one of anvil's public
//! development keys) or one whose key a fork rehearsal made and keeps under
//! `.shonin/mainnet-fork` (the address `chains:fork-admin` prints for the
//! rehearsal's command line), before any chain is read; and a real deploy
//! takes the admin's own key file beside ADMIN, proving its key exists.
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

fn real_deploy(dir: &Path, admin: &str) -> Output {
    let deployer = PrivateKeySigner::random();
    let file = dir.join("deployer");
    std::fs::write(
        &file,
        format!("0x{}\n", alloy::hex::encode(deployer.to_bytes())),
    )
    .unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    devnet(
        dir,
        &[
            "mainnet-deploy",
            "--admin",
            admin,
            "--deployer-keyfile",
            file.to_str().unwrap(),
            "--confirm-mainnet",
            "8453",
            "--mainnet-generation",
            "1",
            "--admin-steps-dir",
            dir.join("steps").to_str().unwrap(),
        ],
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
fn a_real_deploy_refuses_one_of_anvil_s_public_development_keys_as_admin() {
    let dir = scratch("anvil");
    // Anvil's first account: its key is printed by every anvil.
    let output = real_deploy(&dir, "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266");
    let _ = std::fs::remove_dir_all(&dir);
    refused_for_the_admin(&output, &["anvil", "public"]);
}

#[test]
fn a_real_deploy_refuses_a_fork_rehearsal_s_throwaway_admin() {
    let dir = scratch("throwaway");
    // The throwaway the tool makes for rehearsals (made the first time,
    // kept under .shonin/mainnet-fork, and recorded there).
    let made = devnet(&dir, &["mainnet-fork-admin"]);
    assert!(made.status.success(), "{}", said(&made));
    let throwaway = String::from_utf8_lossy(&made.stdout).trim().to_string();
    let output = real_deploy(&dir, &throwaway);
    let _ = std::fs::remove_dir_all(&dir);
    refused_for_the_admin(&output, &["throwaway"]);
}

/// Anvil's first account: its key is printed by every anvil.
const ANVIL_FIRST: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

#[test]
fn the_dry_run_and_the_read_back_refuse_a_public_or_throwaway_admin_too() {
    let dir = scratch("modes");
    let made = devnet(&dir, &["mainnet-fork-admin"]);
    assert!(made.status.success(), "{}", said(&made));
    let throwaway = String::from_utf8_lossy(&made.stdout).trim().to_string();
    let steps = dir.join("steps");
    for (admin, why) in [(ANVIL_FIRST, "anvil"), (throwaway.as_str(), "throwaway")] {
        let dry_run = devnet(
            &dir,
            &[
                "mainnet-deploy",
                "--admin",
                admin,
                "--mainnet-generation",
                "1",
                "--admin-steps-dir",
                steps.to_str().unwrap(),
            ],
        );
        refused_for_the_admin(&dry_run, &[why]);
        let read_back = devnet(
            &dir,
            &[
                "mainnet-verify",
                "--admin",
                admin,
                "--mainnet-generation",
                "1",
            ],
        );
        refused_for_the_admin(&read_back, &[why]);
        let admin_steps = devnet(
            &dir,
            &[
                "mainnet-admin-steps",
                "--admin",
                admin,
                "--admin-steps-dir",
                steps.to_str().unwrap(),
            ],
        );
        refused_for_the_admin(&admin_steps, &[why]);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_real_deploy_takes_the_admin_s_own_key_file_beside_admin() {
    let dir = scratch("keyfile");
    let admin = PrivateKeySigner::random();
    let address = format!("{:?}", admin.address());
    // Without the admin's key file: refused, naming it.
    let output = real_deploy(&dir, &address);
    let text = said(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(
        text.contains("ADMIN_KEYFILE") && text.contains("Nothing was sent"),
        "a real deploy went on without the admin's key file:\n{text}"
    );
    // With a key file holding another key than ADMIN's: refused.
    let other = PrivateKeySigner::random();
    let file = dir.join("admin");
    std::fs::write(
        &file,
        format!("0x{}\n", alloy::hex::encode(other.to_bytes())),
    )
    .unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let deployer = PrivateKeySigner::random();
    let deployer_file = dir.join("deployer");
    std::fs::write(
        &deployer_file,
        format!("0x{}\n", alloy::hex::encode(deployer.to_bytes())),
    )
    .unwrap();
    std::fs::set_permissions(&deployer_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let output = devnet(
        &dir,
        &[
            "mainnet-deploy",
            "--admin",
            &address,
            "--deployer-keyfile",
            deployer_file.to_str().unwrap(),
            "--admin-keyfile",
            file.to_str().unwrap(),
            "--confirm-mainnet",
            "8453",
            "--mainnet-generation",
            "1",
            "--admin-steps-dir",
            dir.join("steps").to_str().unwrap(),
        ],
    );
    let text = said(&output);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(
        text.contains("not ADMIN=") && text.contains("Nothing was sent"),
        "a real deploy took a key file that is not ADMIN's:\n{text}"
    );
}
