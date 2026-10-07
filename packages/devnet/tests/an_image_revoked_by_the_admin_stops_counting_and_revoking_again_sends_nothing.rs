//! `revoke-image`, as `task chains:revoke-image` runs it, against a local
//! chain: an image proposed and past its timelock counts until the admin
//! revokes it, and from then on never does. Run again, it finds the image
//! revoked and sends nothing; asked for a digest never proposed there, it
//! sends nothing either, since such a revocation wouldn't last (proposing
//! the digest later allows it afresh).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const DIGEST: &str = "sha256:5ea1ed0000000000000000000000000000000000000000000000000000c0ffee";
const NEVER_PROPOSED: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000bad";

struct Anvil(Child);

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A free port, never one of the development stack's own.
fn free_port() -> u16 {
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if !(19000..=19200).contains(&port) {
            return port;
        }
    }
}

fn anvil() -> Option<(Anvil, u16)> {
    let port = free_port();
    let child = Command::new("anvil")
        .args([
            "--port",
            &port.to_string(),
            "--chain-id",
            "31337",
            "--silent",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let anvil = Anvil(child);
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        if started.elapsed() > Duration::from_secs(20) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Some((anvil, port))
}

/// A copy of the built contracts (every file under the contracts' `src`),
/// so a build running beside this test changes nothing under it; none if
/// they are not built.
fn artifacts(into: &Path) -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm");
    let built = root.join("out");
    if !built
        .join("LocalVenue.sol")
        .join("LocalVenue.json")
        .exists()
    {
        return None;
    }
    let mut sources = vec![root.join("src")];
    while let Some(dir) = sources.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources.push(path);
                continue;
            }
            let Some(file) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let from = built.join(file);
            if !file.ends_with(".sol") || !from.is_dir() {
                continue;
            }
            let to = into.join(file);
            std::fs::create_dir_all(&to).unwrap();
            for json in std::fs::read_dir(&from).unwrap().flatten() {
                std::fs::copy(json.path(), to.join(json.file_name())).unwrap();
            }
        }
    }
    Some(into.to_path_buf())
}

/// Run the devnet with `command`, as the tasks do, and return whether it
/// succeeded and what it said.
fn devnet(chains: &Path, dir: &Path, artifacts: &Path, command: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_devnet"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .args(["--network", "local", "--vault-chain", "base"])
        .arg("--chains")
        .arg(chains)
        .arg("--deployments-dir")
        .arg(dir.join("deployments"))
        .arg("--store")
        .arg(dir.join("store"))
        .arg("--artifacts")
        .arg(artifacts)
        .args(command)
        .output()
        .expect("the devnet binary");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), said)
}

fn ask(url: &str, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let reply: Value = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            reqwest::Client::new()
                .post(url)
                .json(&body)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap()
        });
    reply["result"].clone()
}

fn word(digest: &str) -> String {
    digest.trim_start_matches("sha256:").to_string()
}

/// `isImageAllowed(bytes32)`.
fn allowed(url: &str, registry: &str, digest: &str) -> bool {
    let selector = &alloy::primitives::keccak256("isImageAllowed(bytes32)")[..4];
    let data = format!("0x{}{}", alloy::hex::encode(selector), word(digest));
    let out = ask(
        url,
        "eth_call",
        json!([{"to": registry, "data": data}, "latest"]),
    );
    out.as_str().unwrap().ends_with('1')
}

fn sends_by(url: &str, who: &str) -> u64 {
    let count = ask(url, "eth_getTransactionCount", json!([who, "latest"]));
    u64::from_str_radix(count.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

#[test]
fn an_image_revoked_by_the_admin_stops_counting_and_revoking_again_sends_nothing() {
    let dir = std::env::temp_dir().join(format!("shonin-revoke-image-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let Some(artifacts) = artifacts(&dir.join("out")) else {
        eprintln!("the contracts are not built: skipping");
        return;
    };
    let Some((_anvil, port)) = anvil() else {
        eprintln!("anvil is not installed: skipping");
        return;
    };
    let url = format!("http://127.0.0.1:{port}");
    let chains = dir.join("chains.toml");
    std::fs::write(
        &chains,
        format!(
            "[chains.\"evm:31337\"]\nlabel = \"a local chain\"\nrpc = \"{url}\"\n\
             dollar = \"USDC\"\nnative_usd = 2500.0\nconfirmations = 0\n\n\
             [networks.local]\nbase = \"evm:31337\"\n"
        ),
    )
    .unwrap();

    let (deployed, said) = devnet(&chains, &dir, &artifacts, &["deploy"]);
    assert!(deployed, "the deploy failed:\n{said}");
    let recorded: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("deployments").join("evm-31337.json")).unwrap(),
    )
    .unwrap();
    let registry = recorded["attestation"].as_str().unwrap().to_string();
    let admin = recorded["roles"]["admin"].as_str().unwrap().to_string();

    let (proposed, said) = devnet(
        &chains,
        &dir,
        &artifacts,
        &["propose-image", "--digest", DIGEST, "--kind", "executor"],
    );
    assert!(proposed, "the proposal failed:\n{said}");
    assert!(
        !allowed(&url, &registry, DIGEST),
        "allowed before its timelock"
    );
    // Past the timelock: the image counts.
    ask(&url, "evm_increaseTime", json!([30 * 24 * 3_600]));
    ask(&url, "evm_mine", json!([]));
    assert!(
        allowed(&url, &registry, DIGEST),
        "not allowed after its timelock"
    );

    let (revoked, said) = devnet(
        &chains,
        &dir,
        &artifacts,
        &["revoke-image", "--digest", DIGEST],
    );
    assert!(revoked, "the revocation failed:\n{said}");
    assert!(said.contains("is revoked"), "{said}");
    assert!(
        !allowed(&url, &registry, DIGEST),
        "still allowed once revoked"
    );

    // Again: already revoked, and nothing sent.
    let before = sends_by(&url, &admin);
    let (again, said) = devnet(
        &chains,
        &dir,
        &artifacts,
        &["revoke-image", "--digest", DIGEST],
    );
    assert!(again, "revoking again failed:\n{said}");
    assert!(said.contains("already revoked"), "{said}");
    assert_eq!(sends_by(&url, &admin), before);
    // However long after.
    ask(&url, "evm_increaseTime", json!([365 * 24 * 3_600]));
    ask(&url, "evm_mine", json!([]));
    assert!(!allowed(&url, &registry, DIGEST), "allowed again in time");

    // A revoked image can't be proposed back.
    let (back, said) = devnet(
        &chains,
        &dir,
        &artifacts,
        &["propose-image", "--digest", DIGEST, "--kind", "executor"],
    );
    assert!(!back, "a revoked image was proposed again:\n{said}");

    // A digest never proposed here: nothing to revoke, nothing sent.
    let (never, said) = devnet(
        &chains,
        &dir,
        &artifacts,
        &["revoke-image", "--digest", NEVER_PROPOSED],
    );
    assert!(never, "{said}");
    assert!(said.contains("never proposed"), "{said}");
    assert_eq!(sends_by(&url, &admin), before);
    let _ = std::fs::remove_dir_all(&dir);
}
