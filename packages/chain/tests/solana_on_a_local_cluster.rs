//! The Solana client against a real local cluster: Surfpool, started for
//! each test on ports of its own, offline. Without Surfpool 1.2.0 or later
//! (`SHONIN_SURFPOOL`, or the one on the path) the tests say so and pass, as
//! CI runs the programs' own tests in-process and has no cluster.

#![cfg(feature = "solana")]

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use chain::solana::keys::{DEVELOPMENT_MNEMONIC, RoleKeys};
use chain::solana::platform::{Platform, ed25519};
use chain::solana::{Keypair, Signer, SolanaClient, TransactionFailed};
use chain::{ChainConfig, ChainKey, ChainKind, Role};

/// A Surfpool cluster that is stopped when dropped.
struct Cluster {
    child: Child,
    _dir: std::path::PathBuf,
    rpc: String,
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self._dir);
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The Surfpool to run: `SHONIN_SURFPOOL`, or the one on the path.
fn surfpool() -> String {
    std::env::var("SHONIN_SURFPOOL").unwrap_or_else(|_| "surfpool".to_string())
}

fn start() -> Option<Cluster> {
    let Ok(version) = Command::new(surfpool()).arg("--version").output() else {
        eprintln!("surfpool is not installed: skipping");
        return None;
    };
    // `surfpool 1.6.0`: before 1.2.0 there is no Ed25519 program offline.
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
    let (rpc, ws) = (free_port(), free_port());
    let dir = std::env::temp_dir().join(format!("shonin-surfpool-{}-{rpc}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let child = Command::new(surfpool())
        .args([
            "start",
            "--offline",
            "--no-tui",
            "--no-studio",
            "--no-deploy",
            "-y",
        ])
        .args(["--port", &rpc.to_string(), "--ws-port", &ws.to_string()])
        .args(["--log-path", "./logs"])
        .current_dir(&dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    Some(Cluster {
        child,
        _dir: dir,
        rpc: format!("http://127.0.0.1:{rpc}"),
    })
}

async fn connect(cluster: &Cluster) -> SolanaClient {
    let config = ChainConfig {
        label: "Solana (test)".into(),
        rpc: cluster.rpc.clone(),
        ws: None,
        dollar: "USDC".into(),
        native_usd: None,
        confirmations: None,
        public_rpc: cluster.rpc.clone(),
    };
    let keys = Arc::new(RoleKeys::from_mnemonic(DEVELOPMENT_MNEMONIC).unwrap());
    SolanaClient::connect_with(
        &ChainKey::new(ChainKind::Solana, "localnet"),
        &config,
        Some(keys),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_send_lands_with_its_fee_and_rent_counted_and_its_accounts_read_back() {
    let Some(cluster) = start() else { return };
    let client = connect(&cluster).await;
    let admin = client.role(Role::Admin).unwrap();
    let viewer = client.role(Role::Viewer).unwrap();
    client
        .airdrop(&admin.pubkey(), 10_000_000_000)
        .await
        .unwrap();
    assert!(client.lamports(&admin.pubkey()).await.unwrap() >= 10_000_000_000);
    assert!(
        client.now().await.unwrap() > 1_700_000_000,
        "the clock reads real time"
    );

    // A mint: a new account the admin pays the rent of.
    let mint = Keypair::new();
    let rent = client
        .rpc
        .rent_exempt_minimum(Platform::rent_for_mint())
        .await
        .unwrap();
    let create =
        Platform::ix_create_mint(&admin.pubkey(), &mint.pubkey(), &admin.pubkey(), rent).unwrap();
    let landed = client.send(create, &[admin, &mint]).await.unwrap();
    assert!(landed.fee() > 0);
    assert_eq!(
        landed.rent(),
        rent as i128,
        "the payer's spend is the fee and the rent"
    );
    let record = client
        .transaction(landed.signature())
        .await
        .unwrap()
        .expect("the node holds what landed");
    assert_eq!(record.fee, landed.fee());

    // Dollars to the viewer, read back from its token account.
    let platform = Platform {
        cluster_name: "localnet".into(),
        cluster: client.cluster_value(),
        mint: mint.pubkey(),
        admin: admin.pubkey(),
        platform: client.address(Role::Platform).unwrap(),
        operator: client.address(Role::Operator).unwrap(),
        checker: None,
    };
    let ixs = vec![
        platform.ix_open_dollars(&admin.pubkey(), &viewer.pubkey()),
        platform
            .ix_mint_to(&admin.pubkey(), &viewer.pubkey(), 25_000_000)
            .unwrap(),
    ];
    client.send(ixs, &[admin]).await.unwrap();
    assert_eq!(
        client
            .token_balance(&platform.dollars(&viewer.pubkey()))
            .await
            .unwrap(),
        25_000_000
    );
    let account = client.account(&mint.pubkey()).await.unwrap().unwrap();
    assert_eq!(
        account.owner,
        chain::solana::programs::anchor_spl::token::ID
    );
}

#[tokio::test]
async fn a_failing_send_comes_back_with_its_program_error_and_nothing_spent() {
    let Some(cluster) = start() else { return };
    let client = connect(&cluster).await;
    let admin = client.role(Role::Admin).unwrap();
    client
        .airdrop(&admin.pubkey(), 10_000_000_000)
        .await
        .unwrap();
    let before = client.lamports(&admin.pubkey()).await.unwrap();
    // Minting from a mint that doesn't exist: the token program refuses.
    let platform = Platform {
        cluster_name: "localnet".into(),
        cluster: client.cluster_value(),
        mint: Keypair::new().pubkey(),
        admin: admin.pubkey(),
        platform: admin.pubkey(),
        operator: admin.pubkey(),
        checker: None,
    };
    let ix = platform
        .ix_mint_into(&admin.pubkey(), &Keypair::new().pubkey(), 1)
        .unwrap();
    let error = client.send(vec![ix], &[admin]).await.unwrap_err();
    assert!(
        error.downcast_ref::<TransactionFailed>().is_some(),
        "a refused send is a failed transaction: {error:#}"
    );
    assert_eq!(client.lamports(&admin.pubkey()).await.unwrap(), before);
}

#[tokio::test]
async fn a_signature_carried_in_a_transaction_is_checked_by_the_ed25519_program() {
    let Some(cluster) = start() else { return };
    let client = connect(&cluster).await;
    let admin = client.role(Role::Admin).unwrap();
    client
        .airdrop(&admin.pubkey(), 10_000_000_000)
        .await
        .unwrap();
    let signer = client.role(Role::Executor).unwrap();
    client
        .send(vec![ed25519(signer, b"a usage total")], &[admin])
        .await
        .unwrap();
    // The same instruction with its message changed after signing: the
    // cluster refuses it, so a program reading it is never fooled.
    let mut forged = ed25519(signer, b"a usage total");
    let last = forged.data.len() - 1;
    forged.data[last] ^= 1;
    assert!(client.send(vec![forged], &[admin]).await.is_err());
}

#[tokio::test]
async fn a_local_cluster_key_refuses_a_deployment_of_another_genesis() {
    let Some(cluster) = start() else { return };
    let client = connect(&cluster).await;
    let stale = chain::SolanaDeployments {
        cluster: "localnet".into(),
        genesis_hash: "an earlier cluster".into(),
        ..Default::default()
    };
    assert!(client.check_deployments(&stale).await.is_err());
    let fresh = chain::SolanaDeployments {
        cluster: "localnet".into(),
        genesis_hash: client.genesis_hash.clone(),
        ..Default::default()
    };
    client.check_deployments(&fresh).await.unwrap();
}
