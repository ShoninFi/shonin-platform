//! The operator's project the admin's key proposes is one the operator
//! named, never one a steps file names: the deploy's steps are rebuilt from
//! the deployment file, a project proposal in the steps file is never
//! signed from the file, and the project named on the command line is
//! proposed as named (the words printed are the rebuilt step's).
//!
//! A plain anvil answering as Base's chain id on a loopback port, signed by
//! the fork's throwaway admin; nothing reaches a public endpoint.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use alloy::primitives::{Address, keccak256};
use alloy::sol_types::SolValue;

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

fn devnet(dir: &Path, port: u16, args: &[&str]) -> Output {
    let chains = dir.join("chains.toml");
    std::fs::write(
        &chains,
        format!(
            "[chains.\"evm:8453\"]\nlabel = \"Base\"\nrpc = \"http://127.0.0.1:{port}\"\n\
             dollar = \"USDC\"\n\n[networks.mainnet]\nbase = \"evm:8453\"\nroot = \"base\"\n"
        ),
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

/// Every transaction input the admin's fork received, by block (anvil
/// mines one a block).
fn inputs_sent(port: u16) -> Vec<String> {
    let height = ask(port, "eth_blockNumber", serde_json::json!([]));
    let height =
        u64::from_str_radix(height.as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
    let mut inputs = Vec::new();
    for number in 1..=height {
        let block = ask(
            port,
            "eth_getBlockByNumber",
            serde_json::json!([format!("0x{number:x}"), true]),
        );
        for tx in block["transactions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            inputs.extend(tx["input"].as_str().map(str::to_string));
        }
    }
    inputs
}

/// `proposeProject(keccak256(project))`, as hex.
fn proposal_of(project: &str) -> String {
    let mut data = keccak256("proposeProject(bytes32)")[..4].to_vec();
    data.extend_from_slice(&keccak256(project).abi_encode());
    format!("0x{}", alloy::hex::encode(&data))
}

/// A deployment on the fork at `port` with no driver recorded (its steps
/// rebuild to the station factory's initialisation alone), and a steps
/// file holding one project proposal whose words name the operator's
/// project and whose calldata proposes another. The scratch directory, the
/// throwaway admin and the steps directory.
fn deployment_with_a_filed_proposal(port: u16) -> (PathBuf, String, PathBuf) {
    let dir = std::env::temp_dir().join(format!("shonin-project-{port}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let made = devnet(&dir, 1, &["mainnet-fork-admin"]);
    assert!(made.status.success(), "{}", said(&made));
    let admin = String::from_utf8_lossy(&made.stdout).trim().to_string();
    let admin_address: Address = admin.parse().unwrap();

    let registry = Address::repeat_byte(0xa1);
    let zero = Address::ZERO;
    let deployments = dir.join("deployments");
    std::fs::create_dir_all(&deployments).unwrap();
    let roles = serde_json::json!({
        "admin": admin, "platform": admin, "attestor": zero, "builder": zero,
        "reviewer": zero, "executor": zero, "keeper": zero, "operator": zero,
        "author": zero, "viewer": zero,
    });
    std::fs::write(
        deployments.join("evm-8453.json"),
        serde_json::json!({
            "chain_key": "evm:8453", "chain_id": 8453, "rpc_url": "", "deployed_block": 0,
            "usd": Address::repeat_byte(0xd0), "venue": zero, "attestation_verifier": zero,
            "attestation": registry, "publishing": zero, "prices": zero,
            "strategies": zero, "reviews": zero, "profiles": zero,
            "factory": Address::repeat_byte(0xfa),
            "station_factory": Address::repeat_byte(0xa2), "driver": zero, "bridge": zero,
            "closer": zero, "roles": roles,
            "station_implementation": Address::repeat_byte(0x57),
            "mainnet": {
                "risc_zero": zero, "vault_implementation": zero, "drivers": {},
                "deployer": Address::repeat_byte(0x44),
                "build": "0x0000000000000000000000000000000000000000000000000000000000000000",
            },
        })
        .to_string(),
    )
    .unwrap();
    // The registry answers every call (a project not yet proposed).
    ask(
        port,
        "anvil_setCode",
        serde_json::json!([registry, "0x6101006000f3"]),
    );

    let steps = dir.join("steps");
    std::fs::create_dir_all(&steps).unwrap();
    std::fs::write(
        steps.join("admin-steps-evm-8453.json"),
        serde_json::json!({
            "chain_id": 8453,
            "admin": admin_address,
            "registry": registry,
            "from_block": 0,
            "steps": [{
                "chain_id": 8453,
                "to": registry,
                "data": proposal_of("someone-else-s-project"),
                "value": "0x0",
                "purpose": "propose the operator's project shonin-fi",
                "kind": "propose-project",
            }]
        })
        .to_string(),
    )
    .unwrap();
    (dir, admin, steps)
}

fn admin_steps(dir: &Path, port: u16, admin: &str, steps: &Path, project: Option<&str>) -> Output {
    let mut args = vec![
        "mainnet-admin-steps",
        "--admin",
        admin,
        "--on-fork",
        "--admin-steps-dir",
        steps.to_str().unwrap(),
    ];
    if let Some(project) = project {
        args.extend(["--project", project]);
    }
    devnet(dir, port, &args)
}

#[test]
fn a_project_proposal_no_one_named_is_never_signed_from_the_steps_file() {
    let port = 18681;
    let _node = anvil(port);
    let (dir, admin, steps) = deployment_with_a_filed_proposal(port);
    let output = admin_steps(&dir, port, &admin, &steps, None);
    let text = said(&output);
    let sent = inputs_sent(port);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !sent.contains(&proposal_of("someone-else-s-project")),
        "the admin signed and sent a proposal of a project no one named (from the steps file \
         alone):\n{text}"
    );
    assert!(
        !text.contains("sent: propose the operator's project"),
        "{text}"
    );
}

#[test]
fn the_project_named_is_proposed_as_named_and_a_steps_file_proposing_another_is_refused() {
    let port = 18682;
    let _node = anvil(port);
    let (dir, admin, steps) = deployment_with_a_filed_proposal(port);

    // Named, beside a steps file proposing another project: refused whole.
    let refused = admin_steps(&dir, port, &admin, &steps, Some("shonin-fi"));
    let text = said(&refused);
    assert_eq!(refused.status.code(), Some(1), "{text}");
    assert!(text.contains("Nothing was sent"), "{text}");
    assert!(inputs_sent(port).is_empty(), "{text}");

    // Named, the steps file holding the deploy's steps alone: the project
    // named is proposed, under the rebuilt step's words.
    std::fs::write(
        steps.join("admin-steps-evm-8453.json"),
        serde_json::json!({
            "chain_id": 8453,
            "admin": admin.parse::<Address>().unwrap(),
            "registry": Address::repeat_byte(0xa1),
            "from_block": 0,
            "steps": [],
        })
        .to_string(),
    )
    .unwrap();
    let output = admin_steps(&dir, port, &admin, &steps, Some("shonin-fi"));
    let text = said(&output);
    let sent = inputs_sent(port);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(output.status.success(), "{text}");
    assert!(sent.contains(&proposal_of("shonin-fi")), "{text}");
    assert!(
        text.contains("sent: propose the operator's project shonin-fi"),
        "{text}"
    );
}
