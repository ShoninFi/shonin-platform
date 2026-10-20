//! `solana-setup`, as `task chains:solana-setup NETWORK=testnet` runs it, on
//! a cluster standing in for devnet: Surfpool, offline, with the programs
//! in place as `task contracts:solana:deploy NETWORK=testnet` leaves them
//! (the checkers and the drivers final, the rest upgradeable by the deployer
//! of a team's mnemonic), and the testnet network's Solana cluster pointed
//! at it.
//!
//! The dollar is Circle's USDC, which nobody here mints: the stand-in
//! cluster is given its mint at its devnet address, as devnet has it. Until
//! it is there the set-up refuses and sends nothing. Then the set-up funds
//! every role by transfer from the deployer, keeping its reserve; takes
//! Circle's USDC as the dollar, minting none and setting up no stand-in
//! venue or driver, which only a local cluster has; sets the registry up
//! with no timelock and allows the operator's real project and no image at
//! all; and writes the deployment file with the chain list's endpoint. Run
//! again, it changes no record and mints no dollar; a role that spent is
//! topped back up to its amount and no other is given anything; the file is
//! the same, its first slot kept. The admin's
//! changes then reach the Solana registry: an image proposed once and
//! revoked, a holder skipped, the project already allowed, the development
//! checker kept while it is the only one. A team phrase that is the
//! published development one is refused.
//!
//! Needs Surfpool 1.2.0 or later (`SHONIN_SURFPOOL`, or the one on the
//! path) and the programs built for the local cluster (`task
//! contracts:solana:build`, so this build and the programs share their
//! keypairs); without them it says so and passes, as CI has no cluster.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use chain::solana::keys::{self, RoleKeys};
use chain::solana::platform::{image_digest, project_id};
use chain::solana::programs::{local_driver, local_venue, orca_driver, registry};
use chain::solana::{Signer, SolanaClient, name};
use chain::{ChainConfig, ChainKey, ChainKind, Role, SolanaDeployments};

/// A valid phrase that is not the development one (BIP-39's own test
/// vector), standing for the team's.
const TEAM: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
const PROJECT: &str = "shonin-testnet";
const DIGEST: &str = "sha256:5ea1ed0000000000000000000000000000000000000000000000000000c0ffee";
/// Circle's USDC on Solana devnet: a public cluster's dollar.
const CIRCLE_USDC: &str = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU";
const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

/// Every program, by its library name, and whether it is deployed final.
const PROGRAMS: [(&str, bool); 11] = [
    ("shonin_dev_verifier", true),
    ("shonin_cs_verifier", true),
    ("shonin_local_driver", true),
    ("shonin_orca_driver", true),
    ("shonin_registry", false),
    ("shonin_publishing", false),
    ("shonin_prices", false),
    ("shonin_profiles", false),
    ("shonin_station", false),
    ("shonin_vault", false),
    ("shonin_local_venue", false),
];

const FUNDING: &str = r#"
[testnet."solana:localnet"]
reserve = 1
default = 0.2
admin = 0.5
keeper = 2
platform = 0
attestor = 0

[hyperliquid]
max_price = 60.0
max_per_run = 3.0
activation_usd = 2.0
"#;

/// A Surfpool cluster and a scratch directory, both gone when dropped.
struct Cluster {
    child: Child,
    dir: PathBuf,
    rpc: String,
    port: u16,
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A free port, never one of the development stack's own.
fn free_port() -> u16 {
    loop {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if !(19000..=19200).contains(&port) {
            return port;
        }
    }
}

fn surfpool() -> String {
    std::env::var("SHONIN_SURFPOOL").unwrap_or_else(|_| "surfpool".to_string())
}

/// The programs as `anchor build` leaves them, if they are there.
fn programs() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/solana/target/deploy");
    PROGRAMS
        .iter()
        .all(|(program, _)| {
            dir.join(format!("{program}.so")).exists()
                && dir.join(format!("{program}-keypair.json")).exists()
        })
        .then_some(dir)
}

fn start() -> Option<Cluster> {
    let Ok(version) = Command::new(surfpool()).arg("--version").output() else {
        eprintln!("surfpool is not installed: skipping");
        return None;
    };
    let version = String::from_utf8_lossy(&version.stdout).to_string();
    let numbers: Vec<u32> = version
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .split('.')
        .filter_map(|part| part.parse().ok())
        .collect();
    if numbers.as_slice() < [1, 2].as_slice() {
        eprintln!("{} is older than 1.2.0: skipping", version.trim());
        return None;
    }
    if programs().is_none() {
        eprintln!("the programs are not built (task contracts:solana:build): skipping");
        return None;
    }
    let (port, ws) = (free_port(), free_port());
    let dir =
        std::env::temp_dir().join(format!("shonin-solana-setup-{}-{port}", std::process::id()));
    std::fs::create_dir_all(dir.join("ledger")).unwrap();
    let child = Command::new(surfpool())
        .args([
            "start",
            "--offline",
            "--no-tui",
            "--no-studio",
            "--no-deploy",
            "-y",
        ])
        .args(["--host", "127.0.0.1"])
        .args(["--port", &port.to_string(), "--ws-port", &ws.to_string()])
        .args(["--log-path", "./logs"])
        .current_dir(dir.join("ledger"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let rpc = format!("http://127.0.0.1:{port}");
    std::fs::write(
        dir.join("chains.toml"),
        format!(
            r#"
[chains."solana:localnet"]
label = "Solana (standing in for devnet)"
rpc = "{rpc}"
ws = "ws://127.0.0.1:{ws}"
dollar = "USDC"

[networks.testnet]
solana = "solana:localnet"
"#
        ),
    )
    .unwrap();
    std::fs::write(dir.join("funding.toml"), FUNDING).unwrap();
    Some(Cluster {
        child,
        dir,
        rpc,
        port,
    })
}

/// A client that only reads, once the cluster answers.
async fn reader(cluster: &Cluster) -> SolanaClient {
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", cluster.port)).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "surfpool never answered"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let config = ChainConfig {
        label: "Solana (test)".into(),
        rpc: cluster.rpc.clone(),
        ws: None,
        dollar: "USDC".into(),
        native_usd: None,
        confirmations: None,
        public_rpc: cluster.rpc.clone(),
        logs_rpc: None,
        log_range: None,
    };
    SolanaClient::connect_with(&ChainKey::new(ChainKind::Solana, "localnet"), &config, None)
        .await
        .unwrap()
}

/// The devnet binary on the testnet network, its Solana cluster the
/// stand-in, with `phrase` as the team's.
fn devnet(cluster: &Cluster, phrase: &str, command: &[&str]) -> Output {
    let dir = &cluster.dir;
    Command::new(env!("CARGO_BIN_EXE_devnet"))
        .args(["--network", "testnet", "--vault-chain", "solana"])
        .arg("--chains")
        .arg(dir.join("chains.toml"))
        .arg("--deployments-dir")
        .arg(dir.join("deployments"))
        .arg("--store")
        .arg(dir.join("store"))
        .arg("--artifacts")
        .arg(dir.join("artifacts"))
        .arg("--funding")
        .arg(dir.join("funding.toml"))
        .args(command)
        .env("SHONIN_TESTNET_SOLANA_MNEMONIC", phrase)
        .env_remove("SHONIN_TESTNET_EVM_MNEMONIC")
        .env_remove("SHONIN_TESTNET_KEEPER_SOLANA_KEY")
        .env_remove("SHONIN_NETWORK")
        .env_remove("SHONIN_VAULT_CHAIN")
        .env_remove("SHONIN_DEPLOYMENTS_JSON")
        .env_remove("SHONIN_GENERATION")
        .output()
        .unwrap()
}

fn said(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn ok(output: Output) -> String {
    let text = said(&output);
    assert!(output.status.success(), "the command failed:\n{text}");
    text
}

/// The programs as the testnet deploy leaves them: the checker and the
/// driver final, the rest upgradeable by the team's deployer, which is
/// funded. They are written into place with Surfpool's `surfnet_setAccount`
/// (the program-data account, then the program): a 1 MB program's buffer
/// writes over RPC outrun a small machine's cluster, and how the bytes got
/// there is not what this tests.
async fn place_programs(cluster: &Cluster, client: &SolanaClient) {
    use chain::solana::platform::program_data_address;
    use chain::solana::programs::anchor_lang::solana_program::bpf_loader_upgradeable;
    let deployer = keys::keypair_from_mnemonic(TEAM, keys::DEPLOYER).unwrap();
    client
        .airdrop(&deployer.pubkey(), 100 * LAMPORTS_PER_SOL)
        .await
        .unwrap();
    let loader = bpf_loader_upgradeable::ID.to_string();
    let set = |address: String, data: Vec<u8>, lamports: u64, executable: bool| {
        set_account(cluster, address, data, lamports, &loader, executable)
    };
    let dir = programs().unwrap();
    for (program, fixed) in PROGRAMS {
        let id = keys::read_keypair(&dir.join(format!("{program}-keypair.json")))
            .unwrap()
            .pubkey();
        let data_address = program_data_address(&id);
        // The loader's program-data header: its tag, the slot, then the
        // upgrade authority (none for a final program), then the program.
        let mut data = vec![3, 0, 0, 0];
        data.extend_from_slice(&0u64.to_le_bytes());
        if fixed {
            data.push(0);
            data.extend_from_slice(&[0; 32]);
        } else {
            data.push(1);
            data.extend_from_slice(deployer.pubkey().as_ref());
        }
        data.extend_from_slice(&std::fs::read(dir.join(format!("{program}.so"))).unwrap());
        let rent = client.rpc.rent_exempt_minimum(data.len()).await.unwrap();
        set(data_address.to_string(), data, rent, false).await;
        let mut account = vec![2, 0, 0, 0];
        account.extend_from_slice(data_address.as_ref());
        let rent = client.rpc.rent_exempt_minimum(account.len()).await.unwrap();
        set(id.to_string(), account, rent, true).await;
    }
}

/// Write an account in place with Surfpool's `surfnet_setAccount`.
async fn set_account(
    cluster: &Cluster,
    address: String,
    data: Vec<u8>,
    lamports: u64,
    owner: &str,
    executable: bool,
) {
    let hex: String = data.iter().map(|byte| format!("{byte:02x}")).collect();
    let answer: serde_json::Value = reqwest::Client::new()
        .post(&cluster.rpc)
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "surfnet_setAccount",
            "params": [address, {
                "lamports": lamports,
                "data": hex,
                "owner": owner,
                "executable": executable,
            }],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(answer.get("error").is_none(), "{answer}");
}

/// Circle's USDC mint, as devnet has it: six decimals, some supply, and a
/// mint authority that is Circle's, not any of the team's keys. An offline
/// cluster can't fetch it, so it is written in place.
async fn place_circle_usdc(cluster: &Cluster, client: &SolanaClient) {
    use chain::solana::programs::anchor_spl::token;
    let circle = [0xc1; 32];
    // The token program's mint layout: an optional mint authority, the
    // supply, the decimals, whether it is initialised, an optional freeze
    // authority.
    let mut data = vec![1, 0, 0, 0];
    data.extend_from_slice(&circle);
    data.extend_from_slice(&1_000_000_000_000u64.to_le_bytes());
    data.push(6);
    data.push(1);
    data.extend_from_slice(&[0; 36]);
    let rent = client.rpc.rent_exempt_minimum(data.len()).await.unwrap();
    set_account(
        cluster,
        CIRCLE_USDC.to_string(),
        data,
        rent,
        &token::ID.to_string(),
        false,
    )
    .await;
}

/// The dollar's mint account's data: a mint that minted nothing more is
/// unchanged.
async fn usdc_mint(client: &SolanaClient) -> Vec<u8> {
    let mint: chain::solana::Pubkey = CIRCLE_USDC.parse().unwrap();
    client.account(&mint).await.unwrap().unwrap().data
}

fn read_file(cluster: &Cluster) -> SolanaDeployments {
    let text =
        std::fs::read_to_string(cluster.dir.join("deployments/solana-localnet.json")).unwrap();
    SolanaDeployments::from_json(&text).unwrap()
}

#[tokio::test]
async fn a_public_cluster_is_set_up_without_placeholders_and_a_second_run_only_tops_roles_up() {
    let Some(cluster) = start() else { return };
    let client = reader(&cluster).await;
    let team = RoleKeys::from_mnemonic(TEAM).unwrap();

    // Before the programs are there, the set-up refuses and sends nothing.
    let early = devnet(&cluster, TEAM, &["solana-setup", "--project", PROJECT]);
    assert!(!early.status.success());
    assert!(
        said(&early).contains("deploy the programs first"),
        "{}",
        said(&early)
    );
    assert_eq!(
        client.lamports(&team.address(Role::Keeper)).await.unwrap(),
        0
    );

    place_programs(&cluster, &client).await;

    // Without Circle's USDC on the cluster the set-up refuses, and sends
    // nothing: no role is given any coin.
    let without_usdc = devnet(&cluster, TEAM, &["solana-setup", "--project", PROJECT]);
    assert!(!without_usdc.status.success());
    assert!(
        said(&without_usdc).contains(CIRCLE_USDC) && said(&without_usdc).contains("is not on"),
        "{}",
        said(&without_usdc)
    );
    for role in [Role::Keeper, Role::Admin, Role::Viewer] {
        assert_eq!(
            client.lamports(&team.address(role)).await.unwrap(),
            0,
            "{role:?}"
        );
    }
    place_circle_usdc(&cluster, &client).await;
    let usdc_before = usdc_mint(&client).await;

    // The published development phrase is never a public cluster's.
    let refused = devnet(
        &cluster,
        keys::DEVELOPMENT_MNEMONIC,
        &["solana-setup", "--project", PROJECT],
    );
    assert!(!refused.status.success());
    assert!(
        said(&refused).contains("development phrase"),
        "{}",
        said(&refused)
    );

    let first = ok(devnet(
        &cluster,
        TEAM,
        &["solana-setup", "--project", PROJECT],
    ));
    assert!(first.contains("set up"), "{first}");
    let written = read_file(&cluster);

    // The file: the roles, Circle's USDC as the dollar, the chain list's
    // endpoint, the programs at this build's ids.
    let roles = written.roles.clone().expect("the roles are written");
    assert_eq!(roles.keeper, team.address(Role::Keeper).to_string());
    assert_eq!(roles.admin, team.address(Role::Admin).to_string());
    assert_eq!(roles.deployer, team.deployer.pubkey().to_string());
    assert_eq!(written.mint.as_deref(), Some(CIRCLE_USDC));
    assert_eq!(written.rpc_url, cluster.rpc);
    assert_eq!(written.cluster, "localnet");
    assert_eq!(written.programs.len(), PROGRAMS.len());
    client.check_deployments(&written).await.unwrap();

    // Every role funded by transfer to exactly its amount; the platform and
    // the attestor, given none, hold none.
    for (role, amount) in [
        (Role::Keeper, 2 * LAMPORTS_PER_SOL),
        (Role::Viewer, LAMPORTS_PER_SOL / 5),
        (Role::Builder, LAMPORTS_PER_SOL / 5),
        (Role::Platform, 0),
        (Role::Attestor, 0),
    ] {
        assert_eq!(
            client.lamports(&team.address(role)).await.unwrap(),
            amount,
            "{role:?}"
        );
    }
    // The admin paid its own fees and the rent of what it proposed.
    let admin_held = client.lamports(&team.address(Role::Admin)).await.unwrap();
    assert!(admin_held > LAMPORTS_PER_SOL / 4 && admin_held < LAMPORTS_PER_SOL / 2);

    // The registry: no timelock, the operator's real project, no image
    // (neither a placeholder nor any other), the stand-in driver proposed.
    let config: registry::state::Config = client
        .load(&registry::read::config_address())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(config.timelock(0), 0);
    assert_eq!(config.admin, team.address(Role::Admin));
    let project = alloy::primitives::keccak256(PROJECT).0;
    let allowed: registry::state::ProjectRecord = client
        .load(&registry::read::project_address(&project))
        .await
        .unwrap()
        .expect("the operator's project is proposed");
    assert!(allowed.is_allowed(client.now().await.unwrap()));
    assert!(
        client
            .account(&registry::read::project_address(&project_id()))
            .await
            .unwrap()
            .is_none(),
        "the placeholder project is not"
    );
    for service in ["executor", "builder", "reviewer", "unwinder"] {
        assert!(
            client
                .account(&registry::read::image_address(&image_digest(service)))
                .await
                .unwrap()
                .is_none(),
            "a placeholder {service} image is proposed"
        );
    }
    // Orca's driver proposed; the stand-in driver, a local cluster's, not.
    let driver_proposed = async |driver: [u8; 32]| {
        client
            .account(&registry::read::approval_address(
                registry::state::ApprovalKind::Driver,
                &driver,
            ))
            .await
            .unwrap()
            .is_some()
    };
    assert!(driver_proposed(orca_driver::NAME).await);
    assert!(
        !driver_proposed(local_driver::NAME).await,
        "the stand-in driver is proposed on a public cluster"
    );
    // No stand-in venue, markets or reserves.
    for at in [
        local_venue::venue_address(),
        local_venue::market_address(&name("BTC")),
        local_venue::market_address(&name("BTC-PERP")),
        local_venue::vault_address(),
    ] {
        assert!(
            client.account(&at).await.unwrap().is_none(),
            "a stand-in venue account is at {at}"
        );
    }
    // Every role's dollar account opened, and no dollar minted to any:
    // Circle's USDC comes from Circle's faucet alone.
    let platform = chain::solana::Platform::from_deployments(&written).unwrap();
    for role in [Role::Author, Role::Viewer, Role::Keeper, Role::Admin] {
        assert_eq!(
            client
                .token_balance(&platform.dollars(&team.address(role)))
                .await
                .unwrap(),
            0,
            "{role:?}"
        );
    }
    assert_eq!(usdc_mint(&client).await, usdc_before);

    // Again: nothing changes.
    let watched = [Role::Admin, Role::Keeper, Role::Viewer, Role::Author];
    let mut funded = Vec::new();
    for role in watched {
        funded.push(client.lamports(&team.address(role)).await.unwrap());
    }
    ok(devnet(
        &cluster,
        TEAM,
        &["solana-setup", "--project", PROJECT],
    ));
    assert_eq!(read_file(&cluster), written);
    assert_eq!(usdc_mint(&client).await, usdc_before);
    for (role, before) in watched.into_iter().zip(funded) {
        let held = client.lamports(&team.address(role)).await.unwrap();
        if role == Role::Admin {
            // It spent on the first run, signs nothing on the second, and is
            // topped back up to exactly its amount.
            assert!(before < LAMPORTS_PER_SOL / 2);
            assert_eq!(held, LAMPORTS_PER_SOL / 2, "{role:?}");
        } else {
            assert_eq!(held, before, "{role:?}");
        }
    }

    // The admin's changes reach the Solana registry.
    let propose = ["propose-image", "--digest", DIGEST, "--kind", "executor"];
    assert!(ok(devnet(&cluster, TEAM, &propose)).contains("is proposed"));
    assert!(ok(devnet(&cluster, TEAM, &propose)).contains("already proposed"));
    let digest: [u8; 32] = alloy::hex::decode(DIGEST.trim_start_matches("sha256:"))
        .unwrap()
        .try_into()
        .unwrap();
    let image: registry::state::ImageRecord = client
        .load(&registry::read::image_address(&digest))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(image.kind, registry::state::ImageKind::Executor);
    assert!(image.is_allowed(client.now().await.unwrap()));
    let as_another = ["propose-image", "--digest", DIGEST, "--kind", "builder"];
    assert!(!devnet(&cluster, TEAM, &as_another).status.success());
    let holder = ["propose-image", "--digest", DIGEST, "--kind", "holder"];
    assert!(ok(devnet(&cluster, TEAM, &holder)).contains("no holder kind"));
    let revoke = ["revoke-image", "--digest", DIGEST];
    assert!(ok(devnet(&cluster, TEAM, &revoke)).contains("is revoked"));
    assert!(ok(devnet(&cluster, TEAM, &revoke)).contains("already revoked"));
    let image: registry::state::ImageRecord = client
        .load(&registry::read::image_address(&digest))
        .await
        .unwrap()
        .unwrap();
    assert!(!image.is_allowed(client.now().await.unwrap()));
    assert!(ok(devnet(&cluster, TEAM, &["propose-project", PROJECT])).contains("already allowed"));
    // The development checker is the only one: it stays.
    let retire = devnet(&cluster, TEAM, &["retire-checker", "--dev"]);
    assert!(!retire.status.success());
    assert!(said(&retire).contains("only checker"), "{}", said(&retire));
    assert!(ok(devnet(&cluster, TEAM, &["allow-dev-checker"])).contains("already allowed"));
}
