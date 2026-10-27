//! The admin's key signs on mainnet only from a file its rules allow, and
//! never shows: a key file that is a symlink, readable by others, inside
//! this repository or a `.shonin` directory, not ADMIN's, the deployer's,
//! one of anvil's public keys or a fork's throwaway is refused on mainnet
//! before any chain is read; `ADMIN_KEYFILE` without `CONFIRM_MAINNET` (and
//! the reverse) is refused; a fork refuses a key that isn't its throwaway;
//! a step already done is not sent again; a vault this deployment's factory
//! didn't make is not killed; and the key's hex never appears in what the
//! tool prints. Each refusal exits 1, says "Nothing was sent" and writes
//! nothing.
//!
//! The tests that reach a chain start their own plain anvil answering as
//! Base's chain id on a loopback port; nothing reaches a public endpoint.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;

struct Anvil(Child);

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn anvil(port: u16) -> Anvil {
    let node = Anvil(
        Command::new("anvil")
            .args([
                "--chain-id",
                "8453",
                "--port",
                &port.to_string(),
                "--silent",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("anvil"),
    );
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return node;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("anvil did not start on {port}");
}

fn ask(port: u16, method: &str, params: serde_json::Value) -> serde_json::Value {
    let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let output = Command::new("curl")
        .args(["-s", "-H", "content-type: application/json", "--data"])
        .arg(body.to_string())
        .arg(format!("http://127.0.0.1:{port}"))
        .output()
        .expect("curl");
    serde_json::from_slice::<serde_json::Value>(&output.stdout).expect("an answer")["result"]
        .clone()
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shonin-admin-key-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn chains_file(dir: &Path, port: u16) -> PathBuf {
    let chains = dir.join("chains.toml");
    std::fs::write(
        &chains,
        format!(
            "[chains.\"evm:8453\"]\nlabel = \"Base\"\nrpc = \"http://127.0.0.1:{port}\"\n\
             dollar = \"USDC\"\n\n[networks.mainnet]\nbase = \"evm:8453\"\nroot = \"base\"\n"
        ),
    )
    .unwrap();
    chains
}

fn devnet(dir: &Path, port: u16, deployments: &Path, args: &[&str]) -> Output {
    let chains = chains_file(dir, port);
    Command::new(env!("CARGO_BIN_EXE_devnet"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .current_dir(repo())
        .args(["--network", "mainnet", "--vault-chain", "base"])
        .arg("--chains")
        .arg(&chains)
        .arg("--deployments-dir")
        .arg(deployments)
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

/// A key file holding `key`, with `mode`.
fn key_file(file: &Path, key: &PrivateKeySigner, mode: u32) {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(file, format!("0x{}\n", alloy::hex::encode(key.to_bytes()))).unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A steps file for `admin` with one step: `data` sent to `to`.
fn steps_file(dir: &Path, admin: Address, to: Address, data: &[u8], kind: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("admin-steps-evm-8453.json"),
        serde_json::json!({
            "chain_id": 8453,
            "admin": admin,
            "registry": to,
            "from_block": 0,
            "steps": [{
                "chain_id": 8453,
                "to": to,
                "data": format!("0x{}", alloy::hex::encode(data)),
                "value": "0x0",
                "purpose": format!("the test's {kind}"),
                "kind": kind,
            }]
        })
        .to_string(),
    )
    .unwrap();
}

/// Every file under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(files_under(&path));
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The fork's throwaway admin, as the tool makes and keeps it.
fn fork_throwaway(dir: &Path) -> PrivateKeySigner {
    let output = devnet(dir, 1, &dir.join("deployments"), &["mainnet-fork-admin"]);
    assert!(output.status.success(), "{}", said(&output));
    let file = repo().join(".shonin/mainnet-fork/evm/throwaway-admin");
    let key: PrivateKeySigner = std::fs::read_to_string(&file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("{:?}", key.address())
    );
    assert!(
        !said(&output).contains(&alloy::hex::encode(key.to_bytes())),
        "the throwaway's key was printed"
    );
    key
}

/// A deployment file for Base recording `admin` and the vault factory
/// `factory`.
fn deployment_file(deployments: &Path, admin: &str, factory: Address) {
    std::fs::create_dir_all(deployments).unwrap();
    let zero = Address::ZERO;
    let roles = serde_json::json!({
        "admin": admin, "platform": admin, "attestor": zero, "builder": zero,
        "reviewer": zero, "executor": zero, "keeper": zero, "operator": zero,
        "author": zero, "viewer": zero,
    });
    std::fs::write(
        deployments.join("evm-8453.json"),
        serde_json::json!({
            "chain_key": "evm:8453", "chain_id": 8453, "rpc_url": "", "deployed_block": 0,
            "usd": zero, "venue": zero, "attestation_verifier": zero,
            "attestation": Address::repeat_byte(0xa1), "publishing": zero, "prices": zero,
            "strategies": zero, "reviews": zero, "profiles": zero, "factory": factory,
            "station_factory": zero, "driver": zero, "bridge": zero, "closer": zero,
            "roles": roles,
        })
        .to_string(),
    )
    .unwrap();
}

fn refused(output: &Output, why: &str) {
    let text = said(output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.contains("Nothing was sent"), "{text}");
    assert!(text.contains(why), "expected {why:?}:\n{text}");
}

#[test]
fn a_mainnet_admin_key_file_its_rules_forbid_is_refused_before_any_chain_is_read() {
    // No node answers on this port: a refusal before any read is the only
    // way the run ends without a connection error.
    let port = 1;
    let dir = scratch("refusals");
    let steps = dir.join("steps");
    let key = PrivateKeySigner::random();
    let admin = format!("{:?}", key.address());
    steps_file(
        &steps,
        key.address(),
        Address::repeat_byte(0xa1),
        &[1],
        "propose-project",
    );
    let before = files_under(&dir);
    let run = |file: &Path, admin: &str, extra: &[&str]| {
        // The steps are prepared for the admin named, so the key's own
        // checks are what refuse.
        steps_file(
            &steps,
            admin.parse().unwrap(),
            Address::repeat_byte(0xa1),
            &[1],
            "propose-project",
        );
        deployment_file(&dir.join("deployments"), admin, Address::ZERO);
        let mut args = vec![
            "mainnet-admin-steps",
            "--admin",
            admin,
            "--admin-keyfile",
            file.to_str().unwrap(),
            "--confirm-mainnet",
            "8453",
            "--admin-steps-dir",
            steps.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        devnet(&dir, port, &dir.join("deployments"), &args)
    };

    let good = dir.join("keys/admin");
    key_file(&good, &key, 0o600);
    let link = dir.join("keys/link");
    std::os::unix::fs::symlink(&good, &link).unwrap();
    refused(&run(&link, &admin, &[]), "symlink");
    let hard = dir.join("keys/hard");
    std::fs::hard_link(&good, &hard).unwrap();
    refused(&run(&hard, &admin, &[]), "hard link");
    std::fs::remove_file(&hard).unwrap();

    let open = dir.join("keys/open");
    key_file(&open, &key, 0o640);
    refused(&run(&open, &admin, &[]), "readable by others");

    let shonin = dir.join(".shonin/admin");
    key_file(&shonin, &key, 0o600);
    refused(&run(&shonin, &admin, &[]), ".shonin");

    let inside = repo().join(format!("admin-key-test-{}", std::process::id()));
    key_file(&inside, &key, 0o600);
    let output = run(&inside, &admin, &[]);
    std::fs::remove_file(&inside).unwrap();
    refused(&output, "inside this repository");

    let other = format!("{:?}", Address::repeat_byte(0x42));
    refused(&run(&good, &other, &[]), "not ADMIN");

    refused(
        &run(
            &good,
            &admin,
            &["--deployer-keyfile", good.to_str().unwrap()],
        ),
        "deployer",
    );

    let anvil_key: PrivateKeySigner =
        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
            .parse()
            .unwrap();
    let public = dir.join("keys/public");
    key_file(&public, &anvil_key, 0o600);
    refused(
        &run(&public, &format!("{:?}", anvil_key.address()), &[]),
        "anvil",
    );

    let throwaway = fork_throwaway(&dir);
    let copy = dir.join("keys/throwaway");
    key_file(&copy, &throwaway, 0o600);
    refused(
        &run(&copy, &format!("{:?}", throwaway.address()), &[]),
        "throwaway",
    );

    // The key file and the confirmation go together.
    steps_file(
        &steps,
        key.address(),
        Address::repeat_byte(0xa1),
        &[1],
        "propose-project",
    );
    deployment_file(&dir.join("deployments"), &admin, Address::ZERO);
    let alone = devnet(
        &dir,
        port,
        &dir.join("deployments"),
        &[
            "mainnet-admin-steps",
            "--admin",
            &admin,
            "--admin-keyfile",
            good.to_str().unwrap(),
            "--admin-steps-dir",
            steps.to_str().unwrap(),
        ],
    );
    refused(&alone, "CONFIRM_MAINNET");
    let confirmed = devnet(
        &dir,
        port,
        &dir.join("deployments"),
        &[
            "mainnet-admin-steps",
            "--admin",
            &admin,
            "--confirm-mainnet",
            "8453",
            "--admin-steps-dir",
            steps.to_str().unwrap(),
        ],
    );
    refused(&confirmed, "ADMIN_KEYFILE");
    let both = devnet(
        &dir,
        port,
        &dir.join("deployments"),
        &[
            "mainnet-admin-steps",
            "--admin",
            &admin,
            "--on-fork",
            "--confirm-mainnet",
            "8453",
            "--admin-steps-dir",
            steps.to_str().unwrap(),
        ],
    );
    refused(&both, "ON_FORK");

    // A fork takes its throwaway alone, never another key.
    let fork = devnet(
        &dir,
        port,
        &dir.join("deployments"),
        &[
            "mainnet-admin-steps",
            "--admin",
            &admin,
            "--admin-keyfile",
            good.to_str().unwrap(),
            "--on-fork",
            "--admin-steps-dir",
            steps.to_str().unwrap(),
        ],
    );
    refused(&fork, "throwaway");

    let after: Vec<PathBuf> = files_under(&dir)
        .into_iter()
        .filter(|file| {
            !file.starts_with(dir.join("keys")) && !file.starts_with(dir.join(".shonin"))
        })
        .collect();
    let before: Vec<PathBuf> = before
        .into_iter()
        .filter(|file| !file.starts_with(dir.join("keys")))
        .collect();
    let written: Vec<&PathBuf> = after
        .iter()
        // What the test itself wrote: the chains, the deployment and the
        // steps it gives each run.
        .filter(|file| {
            !before.contains(file)
                && !file.ends_with("chains.toml")
                && !file.ends_with("evm-8453.json")
                && !file.ends_with("admin-steps-evm-8453.json")
        })
        .collect();
    assert!(written.is_empty(), "a refusal wrote {written:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_fork_signs_as_its_throwaway_admin_sends_only_what_is_not_done_and_never_shows_the_key() {
    let port = 18661;
    let _node = anvil(port);
    let dir = scratch("signs");
    let key = fork_throwaway(&dir);
    let hex = alloy::hex::encode(key.to_bytes());
    let admin = format!("{:?}", key.address());
    // The deployment's registry, a stand-in answering every call with
    // zeros: no root key recorded (its clearing is done) and an image
    // never proposed (its proposal is to do, and lands).
    let registry = Address::repeat_byte(0xa1);
    ask(
        port,
        "anvil_setCode",
        serde_json::json!([registry, "0x6101006000f3"]),
    );
    let steps = dir.join("steps");
    let deployments = dir.join("deployments");
    deployment_file(&deployments, &admin, Address::ZERO);
    let run = |args: &[&str]| {
        let mut all = vec![args[0], "--admin", &admin, "--on-fork"];
        all.extend_from_slice(&args[1..]);
        all.extend_from_slice(&["--admin-steps-dir", steps.to_str().unwrap()]);
        devnet(&dir, port, &deployments, &all)
    };
    let nonce = || {
        ask(
            port,
            "eth_getTransactionCount",
            serde_json::json!([admin, "latest"]),
        )
    };

    // No root key recorded: its clearing is done, and nothing is sent.
    let output = run(&["mainnet-clear-root-key"]);
    let text = said(&output);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("nothing to clear"), "{text}");
    assert!(!text.contains("sent:"), "{text}");
    assert_eq!(nonce(), serde_json::json!("0x0"), "a done step was sent");
    assert!(
        !text.to_ascii_lowercase().contains(&hex),
        "the key was printed"
    );

    // An image never proposed: its proposal is to do, and lands once.
    let digest = format!("sha256:{}", "ab".repeat(32));
    let output = run(&["mainnet-image", "--digest", &digest, "--kind", "executor"]);
    let text = said(&output);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("sent:") && text.contains("transaction 0x"),
        "{text}"
    );
    assert_eq!(
        nonce(),
        serde_json::json!("0x1"),
        "the step was not sent once"
    );
    assert!(
        !text.to_ascii_lowercase().contains(&hex),
        "the key was printed:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_vault_this_deployment_s_factory_did_not_make_is_never_killed() {
    let port = 18662;
    let _node = anvil(port);
    let dir = scratch("kill");
    let key = fork_throwaway(&dir);
    let admin = format!("{:?}", key.address());
    // The factory answers `vaults()` with no vault.
    let factory = Address::repeat_byte(0xfa);
    ask(
        port,
        "anvil_setCode",
        serde_json::json!([factory, "0x602060005260406000f3"]),
    );
    let deployments = dir.join("deployments");
    deployment_file(&deployments, &admin, factory);
    let vault = format!("{:?}", Address::repeat_byte(0xbe));
    let output = devnet(
        &dir,
        port,
        &deployments,
        &[
            "mainnet-kill-vault",
            "--admin",
            &admin,
            "--on-fork",
            "--vault",
            &vault,
            "--admin-steps-dir",
            dir.join("steps").to_str().unwrap(),
        ],
    );
    refused(&output, "is not a vault this deployment's factory");
    assert_eq!(
        ask(
            port,
            "eth_getTransactionCount",
            serde_json::json!([admin, "latest"])
        ),
        serde_json::json!("0x0")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
