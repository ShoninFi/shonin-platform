//! A mainnet command refuses, before it sends anything or writes beside the
//! committed addresses, whatever its rules forbid: a generation other than
//! the mainnet one, a fork rehearsal writing to the committed directory by
//! another spelling of its path, a fork's admin steps kept where the
//! hardware key's are (or the hardware key's where the fork's are), the
//! admin's steps or the read-back judged
//! against a local anvil node without the fork flag, and a recorded
//! deployment file another deployment would overwrite.
//!
//! Each test starts its own plain anvil answering as Base's chain id on a
//! loopback port, and runs the devnet binary against a chains file of its
//! own with an empty environment; nothing reaches a public endpoint.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

const ADMIN: &str = "0x00000000000000000000000000000000000A0d11";

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

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("shonin-mainnet-{name}-{}", std::process::id()));
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

#[test]
fn a_mainnet_deploy_under_a_generation_other_than_1_is_refused_before_any_send() {
    let _node = anvil(18645);
    let dir = scratch("generation");
    let steps = dir.join("steps");
    for generation in ["0", "2"] {
        let output = devnet(
            &dir,
            18645,
            &dir.join(".shonin/mainnet-fork/deployments"),
            &[
                "mainnet-deploy",
                "--admin",
                ADMIN,
                "--mainnet-generation",
                generation,
                "--on-fork",
                "--admin-steps-dir",
                steps.to_str().unwrap(),
            ],
        );
        let text = said(&output);
        assert!(!output.status.success(), "{text}");
        assert!(text.contains("GENERATION"), "{text}");
        assert!(
            !text.contains("deployer 0x"),
            "generation {generation} went on to the sends:\n{text}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_fork_rehearsal_never_writes_the_committed_directory_by_another_spelling() {
    let _node = anvil(18646);
    let dir = scratch("dir");
    let committed = repo().join("config/deployments/mainnet");
    let before: Vec<_> = std::fs::read_dir(&committed)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    let link = dir.join("link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&committed, &link).unwrap();
    let elsewhere = dir.join("config/deployments/mainnet");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::create_dir_all(dir.join("config/deployments/local")).unwrap();
    let steps = dir.join("steps");
    let mut spellings = vec![
        repo().join("config/deployments/local/../mainnet"),
        dir.join("config/deployments/local/../mainnet"),
        dir.join("Config/Deployments/Mainnet"),
    ];
    #[cfg(unix)]
    spellings.push(link.clone());
    for spelling in &spellings {
        let output = devnet(
            &dir,
            18646,
            spelling,
            &[
                "mainnet-deploy",
                "--admin",
                ADMIN,
                "--mainnet-generation",
                "1",
                "--on-fork",
                "--admin-steps-dir",
                steps.to_str().unwrap(),
            ],
        );
        assert!(
            !output.status.success(),
            "{}:\n{}",
            spelling.display(),
            said(&output)
        );
        assert!(said(&output).contains("never under"), "{}", said(&output));
    }
    let after: Vec<_> = std::fs::read_dir(&committed)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        before, after,
        "a fork rehearsal wrote into the committed directory"
    );
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn without_the_fork_flag_the_admin_steps_and_the_read_back_refuse_a_local_anvil_endpoint() {
    let _node = anvil(18647);
    let dir = scratch("steps");
    let steps = dir.join("steps");
    std::fs::create_dir_all(&steps).unwrap();
    std::fs::write(
        steps.join("admin-steps-evm-8453.json"),
        format!(
            r#"{{"chain_id":8453,"admin":"{ADMIN}","registry":"0x00000000000000000000000000000000000000a1","from_block":0,
            "steps":[{{"chain_id":8453,"to":"0x00000000000000000000000000000000000000a1",
            "data":"0x475dc5728d0fcf8c17883f07b7da72f0b37df0c6db3c62be2cadbbd0dd80b50e0db442a8",
            "value":"0x0","purpose":"propose the operator's project","kind":"propose-project"}}]}}"#
        ),
    )
    .unwrap();
    let output = devnet(
        &dir,
        18647,
        &dir.join("deployments"),
        &[
            "mainnet-admin-steps",
            "--admin",
            ADMIN,
            "--admin-steps-dir",
            steps.to_str().unwrap(),
        ],
    );
    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("on this machine"),
        "{}",
        said(&output)
    );
    assert!(
        !steps.join("admin-steps-evm-8453.sh").exists(),
        "the hardware key's script was written from a local node's answers"
    );
    let output = devnet(
        &dir,
        18647,
        &dir.join("deployments"),
        &[
            "mainnet-verify",
            "--admin",
            ADMIN,
            "--mainnet-generation",
            "1",
        ],
    );
    assert!(!output.status.success(), "{}", said(&output));
    assert!(
        said(&output).contains("on this machine"),
        "{}",
        said(&output)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_read_back_requires_the_admin_and_the_generation() {
    let _node = anvil(18648);
    let dir = scratch("verify");
    for args in [
        vec!["mainnet-verify", "--on-fork", "--mainnet-generation", "1"],
        vec!["mainnet-verify", "--on-fork", "--admin", ADMIN],
    ] {
        let output = devnet(&dir, 18648, &dir.join("deployments"), &args);
        let text = said(&output);
        assert!(!output.status.success(), "{text}");
        assert!(text.contains("required"), "{text}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_recorded_deployment_whose_factory_has_no_code_is_refused_not_overwritten() {
    let _node = anvil(18649);
    let dir = scratch("recorded");
    let deployments = dir.join(".shonin/mainnet-fork/deployments");
    std::fs::create_dir_all(&deployments).unwrap();
    let other = |n: u8| format!("0x{}", format!("{n:02x}").repeat(20));
    let zero = other(0);
    let recorded = serde_json::json!({
        "chain_key": "evm:8453", "chain_id": 8453, "rpc_url": "", "deployed_block": 1,
        "usd": other(1), "venue": zero, "attestation_verifier": other(2),
        "attestation": other(3), "publishing": other(4), "prices": other(5),
        "strategies": other(6), "reviews": other(7), "profiles": other(8),
        "factory": other(9), "station_factory": other(10), "driver": zero,
        "bridge": zero, "closer": other(11),
        "roles": {
            "admin": other(12), "platform": other(12), "attestor": zero, "builder": zero,
            "reviewer": zero, "executor": zero, "keeper": zero, "operator": zero,
            "author": zero, "viewer": zero
        },
        "generation": 1
    });
    let file = deployments.join("evm-8453.json");
    let text = serde_json::to_string_pretty(&recorded).unwrap();
    std::fs::write(&file, &text).unwrap();
    let steps = dir.join("steps");
    let output = devnet(
        &dir,
        18649,
        &deployments,
        &[
            "mainnet-deploy",
            "--admin",
            ADMIN,
            "--mainnet-generation",
            "1",
            "--on-fork",
            "--admin-steps-dir",
            steps.to_str().unwrap(),
        ],
    );
    let said = said(&output);
    assert!(!output.status.success(), "{said}");
    assert!(said.contains("never overwritten"), "{said}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The hardware key's admin steps (`.shonin/mainnet/admin-steps`) are built
/// from the steps file's `from_block`; a fork's, with the fork's later block,
/// would hide a proposal the hardware key already made. So a fork rehearsal
/// never writes there, and the mainnet commands never write or read the
/// fork's steps (`.shonin/mainnet-fork`).
#[test]
fn a_fork_s_admin_steps_and_the_hardware_key_s_are_never_kept_in_each_other_s_place() {
    let port = 18650;
    let _node = anvil(port);
    let dir = scratch("steps-place");
    let real_steps = repo().join(".shonin/mainnet/admin-steps");
    let real_existed = real_steps.exists();
    let fork_steps = repo().join(".shonin/mainnet-fork/admin-steps");
    let fork_existed = fork_steps.exists();
    let deploy = |on_fork: bool, steps: &Path| {
        let mut args = vec![
            "mainnet-deploy",
            "--admin",
            ADMIN,
            "--mainnet-generation",
            "1",
            "--admin-steps-dir",
            steps.to_str().unwrap(),
        ];
        if on_fork {
            args.push("--on-fork");
        }
        devnet(
            &dir,
            port,
            &dir.join(".shonin/mainnet-fork/deployments"),
            &args,
        )
    };
    let refusals = [
        deploy(true, &real_steps),
        deploy(true, &repo().join(".shonin/Mainnet/Admin-Steps")),
        deploy(false, &fork_steps),
        devnet(
            &dir,
            port,
            &dir.join("deployments"),
            &[
                "mainnet-admin-steps",
                "--admin",
                ADMIN,
                "--admin-steps-dir",
                fork_steps.to_str().unwrap(),
            ],
        ),
        devnet(
            &dir,
            port,
            &dir.join("deployments"),
            &[
                "mainnet-admin-steps",
                "--admin",
                ADMIN,
                "--on-fork",
                "--admin-steps-dir",
                real_steps.to_str().unwrap(),
            ],
        ),
    ];
    if !real_existed {
        let _ = std::fs::remove_dir_all(&real_steps);
    }
    if !fork_existed {
        let _ = std::fs::remove_dir_all(&fork_steps);
    }
    let _ = std::fs::remove_dir_all(&dir);
    for output in &refusals {
        let text = said(output);
        assert!(!output.status.success(), "{text}");
        assert!(
            !text.contains("deployer 0x"),
            "the run went on to its sends:\n{text}"
        );
        assert!(
            text.contains("admin steps") || text.contains("admin's steps"),
            "{text}"
        );
    }
}
