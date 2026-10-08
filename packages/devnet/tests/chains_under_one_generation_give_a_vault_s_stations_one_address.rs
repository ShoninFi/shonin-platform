//! A deployment's generation exists so that a vault's stations sit at one
//! address on every chain of a network: a chain is refused under another
//! generation than the chains already deployed. A station's address follows
//! from the station factory's address and from the station implementation it
//! was initialised with, and both follow from the build as well as the
//! generation.
//!
//! So a chain deployed later under the generation its neighbours record, from
//! a build whose station changed in between, gets a station factory at the
//! same address naming another implementation: the deploy keeps the first
//! chain as it is, says nothing, and writes two files under one generation
//! whose stations for the same vault are at different addresses. A bridge
//! send to "my own address on the other chain" then goes where no station is.
//!
//! Two local chains here. The first is deployed; the station's creation code
//! then changes (a byte appended, as any edit to the station or to the
//! library its closes live in would change it); the second is deployed with
//! no generation named, so it takes the first's. Either that deploy is
//! refused, or the two chains give a vault's station one address.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The contracts a deploy reads from the build's output.
const CONTRACTS: &[&str] = &[
    "BatchCreate2",
    "LocalUsd",
    "LocalVenue",
    "DevAttestationVerifier",
    "AttestationRegistry",
    "Publishing",
    "Prices",
    "StrategyRegistry",
    "ReviewRegistry",
    "AuthorProfiles",
    "LocalDriver",
    "LocalBridge",
    "StationCloses",
    "Station",
    "StationFactory",
    "Closer",
    "Valuations",
    "Claims",
    "Posts",
    "Upkeep",
    "Vault",
    "VaultFactory",
];

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

fn anvil(chain_id: u64) -> Option<(Anvil, u16)> {
    let port = free_port();
    let child = Command::new("anvil")
        .args([
            "--port",
            &port.to_string(),
            "--chain-id",
            &chain_id.to_string(),
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

/// A copy of the built contracts a deploy reads.
fn artifacts(into: &Path) -> Option<PathBuf> {
    let built = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm/out");
    for name in CONTRACTS {
        let from = built
            .join(format!("{name}.sol"))
            .join(format!("{name}.json"));
        if !from.exists() {
            return None;
        }
        let to = into.join(format!("{name}.sol"));
        std::fs::create_dir_all(&to).unwrap();
        std::fs::copy(&from, to.join(format!("{name}.json"))).unwrap();
    }
    Some(into.to_path_buf())
}

/// Change the station's creation code as a later build would: one byte
/// after its end, which a creation ignores.
fn change_the_station(artifacts: &Path) {
    let path = artifacts.join("Station.sol").join("Station.json");
    let mut artifact: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let code = artifact["bytecode"]["object"].as_str().unwrap().to_string();
    artifact["bytecode"]["object"] = json!(format!("{code}00"));
    std::fs::write(&path, artifact.to_string()).unwrap();
}

/// Deploy one vault chain of the local network, with no generation named.
fn deploy(chains: &Path, dir: &Path, artifacts: &Path, vault_chain: &str) -> (bool, String) {
    let log = dir.join(format!("deploy-{vault_chain}.log"));
    let out = std::fs::File::create(&log).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_devnet"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", std::env::var_os("HOME").unwrap_or_default())
        .args(["--network", "local", "--vault-chain", vault_chain])
        .arg("--chains")
        .arg(chains)
        .arg("--deployments-dir")
        .arg(dir.join("deployments"))
        .arg("--store")
        .arg(dir.join("store"))
        .arg("--artifacts")
        .arg(artifacts)
        .arg("deploy")
        .stdout(out.try_clone().unwrap())
        .stderr(out)
        .spawn()
        .expect("the devnet binary");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() > Duration::from_secs(240) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let said = std::fs::read_to_string(&log).unwrap_or_default();
    (status.is_some_and(|status| status.success()), said)
}

/// What a deployment file records.
fn recorded(dir: &Path, chain_id: u64) -> Value {
    let file = dir.join("deployments").join(format!("evm-{chain_id}.json"));
    serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
}

/// Where a station factory puts the station of `vault` on `vault_chain`.
async fn station_of(port: u16, station_factory: &str, vault_chain: u64, vault: &str) -> String {
    let selector =
        alloy::hex::encode(&alloy::primitives::keccak256("stationOf(uint256,address)")[..4]);
    let data = format!("0x{selector}{vault_chain:064x}{vault:0>64}");
    let answer: Value = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_call",
            "params": [{"to": station_factory, "data": data}, "latest"],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    answer["result"]
        .as_str()
        .unwrap_or_else(|| panic!("the station factory did not answer: {answer}"))
        .to_string()
}

#[test]
fn chains_under_one_generation_give_a_vault_s_stations_one_address() {
    let dir = std::env::temp_dir().join(format!("shonin-review-builds-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let Some(artifacts) = artifacts(&dir.join("out")) else {
        eprintln!("the contracts are not built: skipping");
        return;
    };
    let (Some((_first, first_port)), Some((_second, second_port))) = (anvil(31337), anvil(31338))
    else {
        eprintln!("anvil is not installed: skipping");
        return;
    };
    let chains = dir.join("chains.toml");
    std::fs::write(
        &chains,
        format!(
            "[chains.\"evm:31337\"]\nlabel = \"the first chain\"\nrpc = \"http://127.0.0.1:{first_port}\"\n\
             dollar = \"USDC\"\nnative_usd = 2500.0\nconfirmations = 0\n\n\
             [chains.\"evm:31338\"]\nlabel = \"the second chain\"\nrpc = \"http://127.0.0.1:{second_port}\"\n\
             dollar = \"USDC\"\nnative_usd = 25.0\nconfirmations = 0\n\n\
             [networks.local]\nbase = \"evm:31337\"\nhyperliquid = \"evm:31338\"\n"
        ),
    )
    .unwrap();

    let (finished, said) = deploy(&chains, &dir, &artifacts, "base");
    assert!(finished, "the first chain's deploy:\n{said}");
    let first = recorded(&dir, 31337);

    change_the_station(&artifacts);
    let (finished, said) = deploy(&chains, &dir, &artifacts, "hyperliquid");
    if !finished {
        // Refused: the deploy saw that its build no longer gives the kept
        // chain's addresses, and said so before sending anything.
        assert!(
            said.contains("would not share one address") && said.contains("Nothing was sent"),
            "the second deploy failed, but not for the build it was refused for:\n{said}"
        );
        assert!(
            !dir.join("deployments").join("evm-31338.json").exists(),
            "the refused chain was deployed:\n{said}"
        );
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let second = recorded(&dir, 31338);
    assert_eq!(
        first["generation"], second["generation"],
        "the two chains are under one generation:\n{said}"
    );

    let vault = "7777777777777777777777777777777777777777";
    let (at_first, at_second) = tokio::runtime::Runtime::new().unwrap().block_on(async {
        (
            station_of(
                first_port,
                first["station_factory"].as_str().unwrap(),
                31337,
                vault,
            )
            .await,
            station_of(
                second_port,
                second["station_factory"].as_str().unwrap(),
                31337,
                vault,
            )
            .await,
        )
    });
    assert_eq!(
        at_first, at_second,
        "both chains are recorded under generation {} and the second deploy was not refused, yet a \
         vault's station is at one address on the first chain and another on the second",
        first["generation"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
