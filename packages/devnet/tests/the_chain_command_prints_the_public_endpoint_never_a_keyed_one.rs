//! The tasks that shell out to other tools ask the devnet for the vault
//! chain's endpoint, and print or pass on what it says. A developer's `.env`
//! may point the chain at a paid endpoint whose URL carries a key; what the
//! devnet prints is the endpoint the chain file names, which anyone may see,
//! never that one.

use std::path::Path;
use std::process::Command;

/// The key a paid endpoint's URL carries.
const KEY: &str = "k3y-0f-a-pa1d-endp01nt";
/// The endpoint the chain file names.
const PUBLIC: &str = "http://127.0.0.1:1/public";

fn chain_command(dir: &Path, keyed: Option<&str>) -> std::process::Output {
    let chains = dir.join("chains.toml");
    std::fs::write(
        &chains,
        format!(
            "[chains.\"evm:31337\"]\nlabel = \"a local chain\"\nrpc = \"{PUBLIC}\"\n\
             dollar = \"USDC\"\nnative_usd = 2500.0\nconfirmations = 0\n\n\
             [networks.local]\nbase = \"evm:31337\"\n"
        ),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_devnet"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .args(["--network", "local", "--vault-chain", "base"])
        .arg("--chains")
        .arg(&chains)
        .arg("--deployments-dir")
        .arg(dir.join("deployments"))
        .arg("--store")
        .arg(dir.join("store"))
        .arg("--artifacts")
        .arg(dir.join("out"))
        .arg("chain");
    if let Some(keyed) = keyed {
        command.env("SHONIN_RPC_EVM_31337", keyed);
    }
    command.output().expect("the devnet binary")
}

#[test]
fn the_chain_command_prints_the_public_endpoint_never_a_keyed_one() {
    let dir = std::env::temp_dir().join(format!("shonin-public-rpc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let keyed = format!("https://paid.example/v2/{KEY}");
    let output = chain_command(&dir, Some(&keyed));
    let printed = String::from_utf8_lossy(&output.stdout);
    let logged = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{printed}{logged}");
    assert_eq!(printed.lines().next(), Some(PUBLIC), "{printed}");
    assert!(!printed.contains(KEY), "{printed}");
    assert!(!logged.contains(KEY), "{logged}");

    // Without an override, the same.
    let output = chain_command(&dir, None);
    let printed = String::from_utf8_lossy(&output.stdout);
    assert_eq!(printed.lines().next(), Some(PUBLIC), "{printed}");
    let _ = std::fs::remove_dir_all(&dir);
}
