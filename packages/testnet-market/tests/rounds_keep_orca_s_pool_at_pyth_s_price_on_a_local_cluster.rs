//! The Solana side against a real local cluster: Surfpool, offline, with
//! Orca's mainnet program at its own address (the binary the Solana tests
//! pin), its settings and fee tier as Orca lays them out, a stand-in for
//! Pyth's SOL/USD account, the listed pool made by Orca's program, and the
//! dollar the build lists minted to the market's account.
//!
//! The market opens its seed across the price, then Pyth moves: 2% (inside
//! the seed's inner half: one capped swap takes the pool back within the
//! band), then 8% the other way (past it: the seed is taken out, the pool
//! walked to the price with nothing at it, and the seed opened again around
//! it), and a round at a price the pool is at sends nothing.
//!
//! Needs Surfpool 1.2.0 or later (`SHONIN_SURFPOOL`, or the one on the
//! path) and Orca's binary (`task contracts:solana:fixtures`), and a build
//! with the local cluster's ids (no `devnet` feature); without them it says
//! so and passes, as CI has no cluster.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chain::solana::keys::{self, DEVELOPMENT_MNEMONIC};
use chain::solana::orca::{self as venue, setup};
use chain::solana::platform::Platform;
use chain::solana::programs::anchor_spl::associated_token::spl_associated_token_account::instruction::create_associated_token_account_idempotent;
use chain::solana::programs::anchor_spl::token::spl_token;
use chain::solana::{Keypair, Signer, SolanaClient};
use chain::{ChainConfig, ChainKey, ChainKind};
use testnet_market::health::Health;
use testnet_market::homes::Homes;
use testnet_market::market::Settings;
use testnet_market::orca::{self, SolanaMarket};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const LAMPORTS: u64 = 1_000_000_000;
const PRICE_ONE: u128 = 1_000_000_000_000;

fn orca_binary() -> PathBuf {
    Path::new(ROOT).join(".shonin/fixtures/orca_whirlpool.so")
}

/// A Surfpool cluster that is stopped when dropped.
struct Cluster {
    child: Child,
    dir: PathBuf,
    rpc: String,
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start() -> Option<Cluster> {
    let surfpool = std::env::var("SHONIN_SURFPOOL").unwrap_or_else(|_| "surfpool".to_string());
    let Ok(version) = Command::new(&surfpool).arg("--version").output() else {
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
    if !orca_binary().exists() {
        eprintln!("Orca's binary is not fetched (`task contracts:solana:fixtures`): skipping");
        return None;
    }
    let (rpc, ws) = (free_port(), free_port());
    let dir =
        std::env::temp_dir().join(format!("testnet-market-orca-{}-{rpc}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let child = Command::new(surfpool)
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
        dir,
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
        logs_rpc: None,
        log_range: None,
    };
    let started = Instant::now();
    loop {
        match SolanaClient::connect_with(
            &ChainKey::new(ChainKind::Solana, "localnet"),
            &config,
            None,
        )
        .await
        {
            Ok(client) => return client,
            Err(_) if started.elapsed() < Duration::from_secs(60) => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(error) => panic!("the cluster never answered: {error:#}"),
        }
    }
}

/// The pool's price, in dollars a SOL.
async fn pool_price(client: &SolanaClient) -> f64 {
    let pool = orca::read_pool(client).await.unwrap();
    venue::price_of_sqrt(pool.state.sqrt_price) as f64 / PRICE_ONE as f64
}

/// Pyth's stand-in at `dollars` a SOL, published now.
async fn pyth_at(client: &SolanaClient, dollars: f64) {
    let price = (dollars * 1e6) as u128 * (PRICE_ONE / 1_000_000);
    setup::post_price(client, price, client.now().await.unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn rounds_keep_orca_s_pool_at_pyth_s_price_and_set_the_seed_again_once_it_leaves() {
    let mint = keys::keypair_from_mnemonic(DEVELOPMENT_MNEMONIC, 2).unwrap();
    if mint.pubkey() != venue::DOLLAR_MINT {
        eprintln!("this build lists another network's dollar: skipping");
        return;
    }
    let Some(cluster) = start() else {
        return;
    };
    let client = connect(&cluster).await;
    let payer = Keypair::new();
    let market = Keypair::new();
    client
        .airdrop(&payer.pubkey(), 100 * LAMPORTS)
        .await
        .unwrap();
    client
        .airdrop(&market.pubkey(), 50 * LAMPORTS)
        .await
        .unwrap();

    // Orca, as a local cluster has it.
    let elf = std::fs::read(orca_binary()).unwrap();
    setup::load_program(&client, &elf).await.unwrap();
    setup::put(
        &client,
        &venue::CONFIG,
        venue::WHIRLPOOL_PROGRAM,
        setup::config_data(&payer.pubkey()),
    )
    .await
    .unwrap();
    let spacing = venue::POOLS[0].tick_spacing;
    setup::put(
        &client,
        &venue::fee_tier_address(&venue::CONFIG, spacing),
        venue::WHIRLPOOL_PROGRAM,
        setup::fee_tier_data(spacing, setup::FEE_RATE),
    )
    .await
    .unwrap();

    // The dollar, and 10,000 of it for the market.
    let rent = client
        .rpc
        .rent_exempt_minimum(Platform::rent_for_mint())
        .await
        .unwrap();
    client
        .send(
            Platform::ix_create_mint(&payer.pubkey(), &mint.pubkey(), &payer.pubkey(), rent)
                .unwrap(),
            &[&payer, &mint],
        )
        .await
        .unwrap();
    let dollars = orca::dollars_of(&market.pubkey());
    client
        .send(
            vec![
                create_associated_token_account_idempotent(
                    &payer.pubkey(),
                    &market.pubkey(),
                    &venue::DOLLAR_MINT,
                    &spl_token::ID,
                ),
                spl_token::instruction::mint_to(
                    &spl_token::ID,
                    &venue::DOLLAR_MINT,
                    &dollars,
                    &payer.pubkey(),
                    &[],
                    10_000_000_000,
                )
                .unwrap(),
            ],
            &[&payer],
        )
        .await
        .unwrap();

    // The pool at $150, Pyth there too, and the market's seed across it.
    pyth_at(&client, 150.0).await;
    setup::ensure_pool(&client, &payer, 150 * PRICE_ONE)
        .await
        .unwrap();
    orca::open_seed(&client, &market, 300_000_000)
        .await
        .unwrap();
    let seed = setup::seed_position(&client, &market)
        .await
        .unwrap()
        .expect("the seed is open");
    assert!(seed.liquidity > 0);

    let mut kept = SolanaMarket::new(
        "solana:localnet".into(),
        "Solana (test)".into(),
        connect(&cluster).await,
        market.insecure_clone(),
    )
    .await
    .unwrap();
    let settings = Settings::default();
    let mut homes = Homes::load(None).unwrap();
    let health = Mutex::new(Health::default());

    // 2% up: inside the seed's inner half. Each round's swap is capped at
    // $250; a faucet-sized seed is brought back within a round or two.
    pyth_at(&client, 153.0).await;
    let mut rounds = 0;
    let at = loop {
        kept.keep(&settings, &mut homes, &health).await.unwrap();
        rounds += 1;
        let at = pool_price(&client).await;
        if ((at / 153.0 - 1.0) * 10_000.0).abs() < settings.band_bps || rounds == 4 {
            break at;
        }
    };
    assert!(
        ((at / 153.0 - 1.0) * 10_000.0).abs() < settings.band_bps,
        "the pool is at {at} for Pyth's 153 after {rounds} rounds"
    );
    let same_seed = setup::seed_position(&client, &market)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (same_seed.tick_lower, same_seed.tick_upper),
        (seed.tick_lower, seed.tick_upper),
        "the seed stays where it is"
    );

    // 8% down from 153: past the inner half. The seed is set again around
    // the price the pool was walked to.
    pyth_at(&client, 140.0).await;
    kept.keep(&settings, &mut homes, &health).await.unwrap();
    let at = pool_price(&client).await;
    assert!(
        ((at / 140.0 - 1.0) * 10_000.0).abs() < settings.band_bps,
        "the pool is at {at} for Pyth's 140"
    );
    let moved = setup::seed_position(&client, &market)
        .await
        .unwrap()
        .expect("the seed is open again");
    assert!(moved.liquidity > 0);
    let tick = venue::tick_at_or_below(venue::sqrt_of_price(140 * PRICE_ONE));
    assert!(
        moved.tick_lower < tick && tick < moved.tick_upper,
        "the seed spans the price"
    );
    assert!(!setup::needs_recentring(&moved, 140 * PRICE_ONE));

    // At the price: nothing is sent.
    let before = client.lamports(&market.pubkey()).await.unwrap();
    kept.keep(&settings, &mut homes, &health).await.unwrap();
    assert_eq!(client.lamports(&market.pubkey()).await.unwrap(), before);
    let health = health.lock().unwrap();
    let chain = &health.chains["solana:localnet"];
    assert_eq!(chain.error, None);
    assert_eq!(chain.pools.len(), 1);
    assert!(chain.pools[0].last_trade.is_some());
}
