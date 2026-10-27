//! The admin's key signs only the admin's known calls, each aimed at the
//! contract of this deployment its kind calls (read from the deployment
//! file, never from the steps file): a step whose target is none of this
//! deployment's contracts, or whose calldata is not the call its kind
//! makes, is never signed, whatever the steps file says.
//!
//! A plain anvil answering as Base's chain id on a loopback port, signed by
//! the fork's throwaway admin; nothing reaches a public endpoint.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use alloy::primitives::{Address, keccak256};

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

#[test]
fn a_step_aimed_at_no_contract_of_this_deployment_is_never_signed() {
    let mut data = keccak256("revokeImage(bytes32)")[..4].to_vec();
    data.extend_from_slice(&[7u8; 32]);
    refused_unsigned(
        18671,
        Some(Address::repeat_byte(0xbb)),
        "revoke-image",
        &data,
    );
}

#[test]
fn a_step_whose_calldata_is_not_its_kind_s_call_is_never_signed() {
    // Aimed at this deployment's registry, named a driver's proposal, but
    // its calldata revokes an image.
    let mut data = keccak256("revokeImage(bytes32)")[..4].to_vec();
    data.extend_from_slice(&[7u8; 32]);
    refused_unsigned(18672, None, "propose-driver", &data);
}

/// A steps file holding one step of `kind` with `data`, sent to `to` (or
/// to this deployment's registry): the admin signs nothing, and says
/// nothing was sent.
fn refused_unsigned(port: u16, to: Option<Address>, kind: &str, data: &[u8]) {
    let _node = anvil(port);
    let dir =
        std::env::temp_dir().join(format!("shonin-known-calls-{port}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let made = devnet(&dir, 1, &["mainnet-fork-admin"]);
    assert!(made.status.success(), "{}", said(&made));
    let admin = String::from_utf8_lossy(&made.stdout).trim().to_string();
    let admin_address: Address = admin.parse().unwrap();

    // This deployment: its registry at 0xa1…, its station factory at 0xa2….
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
            "usd": zero, "venue": zero, "attestation_verifier": zero,
            "attestation": registry, "publishing": zero, "prices": zero,
            "strategies": zero, "reviews": zero, "profiles": zero, "factory": zero,
            "station_factory": Address::repeat_byte(0xa2), "driver": zero, "bridge": zero,
            "closer": zero, "roles": roles,
        })
        .to_string(),
    )
    .unwrap();

    // The step's target answers every call with zeros (so the step's own
    // check would read "not done").
    let elsewhere = to.unwrap_or(registry);
    ask(
        port,
        "anvil_setCode",
        serde_json::json!([elsewhere, "0x6101006000f3"]),
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
                "to": elsewhere,
                "data": format!("0x{}", alloy::hex::encode(data)),
                "value": "0x0",
                "purpose": "a step the deploy never wrote",
                "kind": kind,
            }]
        })
        .to_string(),
    )
    .unwrap();

    let output = devnet(
        &dir,
        port,
        &[
            "mainnet-admin-steps",
            "--admin",
            &admin,
            "--on-fork",
            "--admin-steps-dir",
            steps.to_str().unwrap(),
        ],
    );
    let text = said(&output);
    let nonce = ask(
        port,
        "eth_getTransactionCount",
        serde_json::json!([admin, "latest"]),
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        nonce,
        serde_json::json!("0x0"),
        "the admin signed and sent a {kind} step to {elsewhere} with calldata it doesn't allow:\n{text}"
    );
    assert!(text.contains("Nothing was sent"), "{text}");
}
