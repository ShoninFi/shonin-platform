//! The Orca half against a real local cluster: Surfpool, offline, with the
//! platform's programs and the Orca driver placed at their ids, Orca's
//! mainnet program at its own address (the binary the Solana tests pin), a
//! stand-in for Pyth's SOL/USD account, and the listed pool made by Orca's
//! program with the market operator's depth, as the devnet sets a local
//! cluster up.
//!
//! A vault pinning the Orca driver is activated and funded; the executor's
//! half buys SOL and opens a band of liquidity through the station's guard,
//! answering the agent as it would; the keeper values the vault through the
//! driver; and a depositor's withdrawal larger than the vault's cash is
//! booked as a claim and served in two parts, half then the rest, each by
//! closing its share at Orca.
//!
//! Needs Surfpool 1.2.0 or later (`SHONIN_SURFPOOL`, or the one on the
//! path), the programs built for the local cluster against the keypairs this
//! build compiled in (`task contracts:solana:build`, or
//! `SHONIN_SOLANA_PROGRAMS`) and Orca's binary (`task
//! contracts:solana:fixtures`); without them it says so and passes, as CI
//! has no cluster.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::time::{Duration, Instant};

use chain::solana::keys::{self, DEVELOPMENT_MNEMONIC, RoleKeys};
use chain::solana::orca::{self as venue, setup};
use chain::solana::platform::{
    Declared, Platform, Registration, VaultAccounts, image_digest, program_data_address,
};
use chain::solana::programs::{anchor_lang, common, orca_driver, publishing, registry, vault};
use chain::solana::rpc::Account;
use chain::solana::{Keypair, Landed, Signer, SolanaClient, TransactionFailed, name};
use chain::{ChainConfig, ChainKey, ChainKind, Role};
use hyperliquid::agent::Outcome;
use strategy_sdk::decimal;
use strategy_sdk::orca::{AccountState, Action, Receipt, Side};

use super::Trading;

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
/// Every program the test places, and whether it is final, as every checker
/// and driver must be.
const PROGRAMS: [(&str, bool); 8] = [
    ("shonin_dev_verifier", true),
    ("shonin_orca_driver", true),
    ("shonin_registry", false),
    ("shonin_publishing", false),
    ("shonin_prices", false),
    ("shonin_profiles", false),
    ("shonin_station", false),
    ("shonin_vault", false),
];
const LAMPORTS: u64 = 1_000_000_000;
const DOLLAR: u64 = 1_000_000;
const DEPOSIT: u64 = 30_000 * DOLLAR;
const PATIENCE: Duration = Duration::from_secs(240);

/// The built programs: `SHONIN_SOLANA_PROGRAMS`, as the devnet takes it, or
/// where `task contracts:solana:build` leaves them.
fn deployed() -> PathBuf {
    std::env::var_os("SHONIN_SOLANA_PROGRAMS")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(ROOT).join("contracts/solana/target/deploy"))
}

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
    if let Some((missing, _)) = PROGRAMS
        .iter()
        .find(|(program, _)| !deployed().join(format!("{program}.so")).exists())
    {
        eprintln!("{missing} is not built (`task contracts:solana:build`): skipping");
        return None;
    }
    if !orca_binary().exists() {
        eprintln!("Orca's binary is not fetched (`task contracts:solana:fixtures`): skipping");
        return None;
    }
    let (rpc, ws) = (free_port(), free_port());
    let dir = std::env::temp_dir().join(format!("shonin-orca-{}-{rpc}", std::process::id()));
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
    let keys = Arc::new(RoleKeys::from_mnemonic(DEVELOPMENT_MNEMONIC).unwrap());
    // The node answers once it is up.
    let started = Instant::now();
    loop {
        match SolanaClient::connect_with(
            &ChainKey::new(ChainKind::Solana, "localnet"),
            &config,
            Some(keys.clone()),
        )
        .await
        {
            Ok(client) => return client,
            Err(error) if started.elapsed() < Duration::from_secs(60) => {
                let _ = error;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(error) => panic!("the cluster never answered: {error:#}"),
        }
    }
}

/// A program at its id as the upgradeable loader keeps one: final, or
/// upgradeable by `deployer`. Written in place rather than deployed: how
/// the bytes got there is not what this tests.
async fn place(client: &SolanaClient, program: &str, fixed: bool, deployer: &Keypair) {
    let id = chain::solana::programs::compiled()
        .into_iter()
        .find(|(name, _)| *name == program)
        .map(|(_, id)| id)
        .unwrap();
    let elf = std::fs::read(deployed().join(format!("{program}.so"))).unwrap();
    let loader = anchor_lang::solana_program::bpf_loader_upgradeable::ID;
    let data_address = program_data_address(&id);
    let mut data = vec![3, 0, 0, 0];
    data.extend_from_slice(&0u64.to_le_bytes());
    if fixed {
        data.push(0);
    } else {
        data.push(1);
        data.extend_from_slice(deployer.pubkey().as_ref());
    }
    // The loader's header is the same size either way.
    data.resize(45, 0);
    data.extend_from_slice(&elf);
    let mut account = vec![2, 0, 0, 0];
    account.extend_from_slice(data_address.as_ref());
    for (address, bytes, executable) in [(data_address, data, false), (id, account, true)] {
        let lamports = client.rpc.rent_exempt_minimum(bytes.len()).await.unwrap();
        client
            .rpc
            .set_account(
                &address,
                &Account {
                    lamports,
                    owner: loader,
                    executable,
                    data: bytes,
                },
            )
            .await
            .unwrap();
    }
}

fn terms() -> vault::state::Terms {
    use vault::state::{Fees, Terms, Timers};
    Terms {
        fees: Fees {
            holding_bps: 100,
            author_bps: 1_000,
            platform_bps: 500,
            transaction_bps: 10,
        },
        exit_band_bps: 100,
        minimum_deposit: 10_000_000,
        minimum_tank: 40_000_000,
        dead_assets: 10_000,
        timers: Timers {
            slot_interval: 10,
            slot_lead: 5,
            slot_window: 3,
            fallback_after: 86_400,
            usage_freshness: 86_400,
            tank_escape_delay: 86_400,
            floor_delay: 86_400,
            halt_grace: 600,
            freeze_after: 7 * 86_400,
            wind_down: 7 * 86_400,
        },
    }
}

const ROLES: [Role; 10] = [
    Role::Admin,
    Role::Platform,
    Role::Attestor,
    Role::Builder,
    Role::Reviewer,
    Role::Executor,
    Role::Keeper,
    Role::Operator,
    Role::Author,
    Role::Viewer,
];

/// The platform as the devnet sets a local cluster up, with Orca's pool and
/// the operator's depth, and the Orca driver approved.
async fn set_up(client: &SolanaClient) -> Platform {
    let keys = client.keys().unwrap();
    let deployer = &keys.deployer;
    let mut everyone = vec![deployer.pubkey()];
    everyone.extend(ROLES.iter().map(|role| keys.address(*role)));
    for who in &everyone {
        client.airdrop(who, 5_000 * LAMPORTS).await.unwrap();
    }
    for (program, fixed) in PROGRAMS {
        place(client, program, fixed, deployer).await;
    }
    let admin = keys.get(Role::Admin);
    let operator = keys.get(Role::Operator);
    // The local dollar, at the development key the driver lists it at.
    let mint = keys::keypair_from_mnemonic(DEVELOPMENT_MNEMONIC, 2).unwrap();
    assert_eq!(mint.pubkey(), venue::DOLLAR_MINT);
    let (create_checker, checker) = Platform::ix_create_checker(
        &deployer.pubkey(),
        keys.address(Role::Attestor),
        common::digest::keccak(&[b"shonin-dev-checker"]),
        &client.cluster,
    );
    let platform = Platform {
        cluster_name: client.cluster.clone(),
        cluster: client.cluster_value(),
        mint: mint.pubkey(),
        admin: admin.pubkey(),
        platform: keys.address(Role::Platform),
        operator: operator.pubkey(),
        checker: Some(checker),
    };
    let rent = client
        .rpc
        .rent_exempt_minimum(Platform::rent_for_mint())
        .await
        .unwrap();
    let ixs = Platform::ix_create_mint(&deployer.pubkey(), &mint.pubkey(), &admin.pubkey(), rent)
        .unwrap();
    client.send(ixs, &[deployer, &mint]).await.unwrap();
    client
        .send(vec![create_checker], &[deployer])
        .await
        .unwrap();
    let registry = Platform::ix_initialize_registry(
        &deployer.pubkey(),
        registry::InitializeArgs {
            admin: admin.pubkey(),
            cluster_name: client.cluster.clone(),
            timelock: 0,
            min_timelock: 0,
            blessing_ttl: 72 * 3600,
        },
        checker,
    );
    for ix in [
        registry,
        platform.ix_initialize_publishing(&deployer.pubkey(), 1_000_000),
        platform.ix_initialize_prices(
            &deployer.pubkey(),
            30 * 86_400,
            chain::solana::programs::prices::UnitPrices {
                vcpu_hour: 150_000_000_000_000_000,
                gib_hour: 10_000_000_000_000_000,
                gb_egress: 150_000_000_000_000_000,
                inference_second: 150_000_000_000_000_000 / 3600,
            },
        ),
        platform.ix_initialize_stations(&deployer.pubkey()),
        platform.ix_initialize_vaults(&deployer.pubkey(), terms()),
    ] {
        client.send(vec![ix], &[deployer]).await.unwrap();
    }
    let mut proposals = Vec::new();
    for (service, kind) in [
        ("executor", registry::state::ImageKind::Executor),
        ("builder", registry::state::ImageKind::Builder),
        ("reviewer", registry::state::ImageKind::Reviewer),
    ] {
        proposals.push(platform.ix_propose_image(image_digest(service), kind));
    }
    proposals.push(platform.ix_propose_project(chain::solana::platform::project_id()));
    client.send(proposals, &[admin]).await.unwrap();
    let propose = platform
        .ix_propose(
            client,
            registry::state::ApprovalKind::Driver,
            orca_driver::NAME,
            orca_driver::ID,
        )
        .await
        .unwrap();
    client.send(vec![propose], &[admin]).await.unwrap();
    let open: Vec<_> = ROLES
        .iter()
        .map(|role| platform.ix_open_dollars(&deployer.pubkey(), &keys.address(*role)))
        .collect();
    client.send(open, &[deployer]).await.unwrap();
    let minting: Vec<_> = [Role::Author, Role::Viewer, Role::Keeper]
        .iter()
        .map(|role| {
            platform
                .ix_mint_to(&admin.pubkey(), &keys.address(*role), 1_000_000 * DOLLAR)
                .unwrap()
        })
        .collect();
    client.send(minting, &[admin]).await.unwrap();

    // Orca, as the devnet sets it up on a local cluster.
    let elf = std::fs::read(orca_binary()).unwrap();
    setup::load_program(client, &elf).await.unwrap();
    setup::put(
        client,
        &venue::CONFIG,
        venue::WHIRLPOOL_PROGRAM,
        setup::config_data(&operator.pubkey()),
    )
    .await
    .unwrap();
    let spacing = venue::POOLS[0].tick_spacing;
    setup::put(
        client,
        &venue::fee_tier_address(&venue::CONFIG, spacing),
        venue::WHIRLPOOL_PROGRAM,
        setup::fee_tier_data(spacing, setup::FEE_RATE),
    )
    .await
    .unwrap();
    let start = 150 * setup::PRICE_ONE;
    setup::post_price(client, start, client.now().await.unwrap())
        .await
        .unwrap();
    let pool = setup::ensure_pool(client, deployer, start).await.unwrap();
    setup::ensure_arrays_around(client, deployer, &pool, 3)
        .await
        .unwrap();
    // An interrupted setup can land the open without its liquidity increase.
    // Resume that exact deterministic position rather than treating existence
    // as completed funding.
    let mint = setup::seed_mint(operator);
    let ticks = venue::ticks_between(start * 9 / 10, start * 11 / 10, spacing).unwrap();
    let open = venue::ix_open_position(
        &operator.pubkey(),
        &operator.pubkey(),
        &mint.pubkey(),
        &pool.address,
        ticks,
    );
    client.send(vec![open], &[operator, &mint]).await.unwrap();
    let empty = setup::seed_position(client, operator)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(empty.liquidity, 0);
    setup::seed(client, &platform, admin, operator, 2_000 * LAMPORTS, 1_000)
        .await
        .unwrap();
    let funded = setup::seed_position(client, operator)
        .await
        .unwrap()
        .unwrap();
    assert!(funded.liquidity > 0);
    assert_eq!((funded.tick_lower, funded.tick_upper), ticks);
    setup::seed(client, &platform, admin, operator, 2_000 * LAMPORTS, 1_000)
        .await
        .unwrap();
    assert_eq!(
        setup::seed_position(client, operator)
            .await
            .unwrap()
            .unwrap()
            .liquidity,
        funded.liquidity,
        "a completed seed is not funded twice"
    );
    platform
}

/// A strategy trading at Orca, registered and reviewed.
async fn publish(client: &SolanaClient, platform: &Platform) -> (u64, [u8; 32]) {
    let role = |role| client.role(role).unwrap();
    let (author, builder, reviewer, attestor) = (
        role(Role::Author),
        role(Role::Builder),
        role(Role::Reviewer),
        role(Role::Attestor),
    );
    let digest = |part: &str| common::digest::keccak(&[part.as_bytes(), b"orca"]);
    for (service, key) in [("builder", builder), ("reviewer", reviewer)] {
        platform
            .bless_service(client, key, attestor, service, &key.pubkey())
            .await
            .unwrap();
    }
    let (submit, id) = platform
        .ix_submit(client, &author.pubkey(), digest("upload"))
        .await
        .unwrap();
    client.send(vec![submit], &[author]).await.unwrap();
    let registration = Registration {
        id,
        author: author.pubkey(),
        source_hash: digest("source"),
        bundle_hash: digest("bundle"),
        manifest_hash: digest("manifest"),
        lists: publishing::state::Lists {
            chains: vec![publishing::state::ChainNeed {
                chain: name("solana"),
                drivers: vec![orca_driver::NAME],
            }],
            bridges: vec![],
            restricted: vec![],
        },
    };
    platform
        .register(client, builder, &registration, builder, reviewer)
        .await
        .unwrap();
    let review = publishing::state::ReviewInput {
        bundle_hash: registration.bundle_hash,
        source_hash: registration.source_hash,
        model_id_hash: digest("model"),
        prompt_hash: digest("prompt"),
        report_hash: digest("report"),
        ratings: [3; 6],
        finding_count: 0,
    };
    let ix = platform
        .ix_submit_review(client, &reviewer.pubkey(), &reviewer.pubkey(), id, review)
        .await
        .unwrap();
    client.send(vec![ix], &[reviewer]).await.unwrap();
    (id, registration.bundle_hash)
}

/// This slot's valuation, if one is due, and every settlement owed.
async fn keep(client: &SolanaClient, platform: &Platform, v: &VaultAccounts) -> Option<Landed> {
    let keeper = client.role(Role::Keeper).unwrap();
    let valued = match platform.value(client, keeper, v).await {
        Ok(landed) => Some(landed),
        Err(error) if error.downcast_ref::<TransactionFailed>().is_some() => None,
        Err(error) => panic!("a valuation could not be sent: {error:#}"),
    };
    platform.settle_all(client, keeper, v).await.unwrap();
    valued
}

fn accepted(outcome: Outcome<Vec<u8>>) -> Receipt {
    match outcome {
        Outcome::Accepted(receipt) => strategy_sdk::from_payload(&receipt).unwrap(),
        Outcome::Refused(why) => panic!("refused: {why}"),
        Outcome::Unknown(why) => panic!("unanswered: {why}"),
    }
}

async fn state(trading: &Trading) -> AccountState {
    strategy_sdk::from_payload(&trading.orca_state().await.unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_orca_vault_trades_through_the_guard_is_valued_through_the_driver_and_a_claim_closes_its_share_at_orca()
 {
    let Some(cluster) = start() else { return };
    let client = connect(&cluster).await;
    let platform = set_up(&client).await;
    let role = |role| client.role(role).unwrap();
    let (author, viewer, executor, attestor, keeper) = (
        role(Role::Author),
        role(Role::Viewer),
        role(Role::Executor),
        role(Role::Attestor),
        role(Role::Keeper),
    );
    let (id, bundle) = publish(&client, &platform).await;

    // Activated on the Orca driver.
    let drivers = [Declared {
        name: orca_driver::NAME,
        program: orca_driver::ID,
    }];
    let (_, statement) = platform
        .restricted_for(&client, &author.pubkey(), id, &drivers)
        .await
        .unwrap();
    let factory = platform.factory(&client).await.unwrap();
    let activate = platform
        .ix_activate(
            &client,
            &author.pubkey(),
            id,
            &drivers,
            Default::default(),
            500,
            factory.terms.minimum_tank,
            statement,
            vault::factory::terms_hash(&factory.terms),
        )
        .await
        .unwrap();
    client.send(vec![activate], &[author]).await.unwrap();
    let v = platform.vault_accounts(id);

    // A depositor in, priced by valuations through the driver with nothing
    // held at Orca yet.
    let statement = platform
        .vault(&client, &v.vault)
        .await
        .unwrap()
        .restricted_hash;
    let deposit = platform.ix_request_deposit(
        &v.vault,
        &viewer.pubkey(),
        &viewer.pubkey(),
        DEPOSIT,
        Some(statement),
        1,
    );
    client.send(vec![deposit], &[viewer]).await.unwrap();
    let started = Instant::now();
    while platform
        .shares(&client, &v.vault, &viewer.pubkey())
        .await
        .unwrap()
        == 0
    {
        keep(&client, &platform, &v).await;
        assert!(started.elapsed() < PATIENCE, "the deposit was never priced");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // The executor's key, blessed and approved, brings dollars to the
    // station; the Orca half is taken up for the station's driver.
    platform
        .bless_executor(
            &client,
            executor,
            attestor,
            &executor.pubkey(),
            &v.vault,
            &bundle,
        )
        .await
        .unwrap();
    let approve = platform
        .ix_approve_key(
            &client,
            &v,
            &bundle,
            &executor.pubkey(),
            0,
            &executor.pubkey(),
        )
        .await
        .unwrap();
    client.send(vec![approve], &[executor]).await.unwrap();
    let to_station = platform
        .ix_move(
            &client,
            &v,
            &bundle,
            &executor.pubkey(),
            10_000 * DOLLAR,
            true,
        )
        .await
        .unwrap();
    client.send(vec![to_station], &[executor]).await.unwrap();
    let spent = Arc::new(AtomicI64::new(0));
    let trading = Trading::new(
        client.clone(),
        platform.clone(),
        Arc::new(executor.insecure_clone()),
        v.clone(),
        bundle,
        spent.clone(),
    );
    let (events, mut heard) = tokio::sync::mpsc::unbounded_channel();
    trading.take_up_orca(events).await.unwrap();
    assert!(trading.has_orca());
    let now = || async { client.now().await.unwrap() };

    // A buy of SOL, then a band around the price with all of it and as many
    // dollars: each answered as the agent reads it, and heard as a change.
    let buy = Action::Swap {
        pool: "SOL/USD".into(),
        side: Side::Buy,
        amount: decimal("2000"),
        limit_price: decimal("151.4"),
    };
    let receipt = accepted(trading.orca_submit(&strategy_sdk::to_payload(&buy)).await);
    let Receipt::Moved { token, dollars } = receipt else {
        panic!("a buy moves tokens: {receipt:?}");
    };
    assert_eq!(dollars, decimal("2000"), "the dollars went into the pool");
    assert!(
        token < decimal("-13") && token > decimal("-13.4"),
        "about 13.3 SOL came out: {token}"
    );
    assert!(heard.try_recv().is_ok(), "the agent heard the change");
    // Pyth follows the pool, as arbitrage would.
    setup::price_follows_pool(&client, now().await)
        .await
        .unwrap();
    let before = state(&trading).await;
    assert_eq!(before.token("SOL"), -token);
    assert_eq!(before.station_idle, decimal("8000"));
    let open = Action::Open {
        pool: "SOL/USD".into(),
        lower_price: decimal("140"),
        upper_price: decimal("160"),
        token: before.token("SOL"),
        dollars: decimal("2000"),
    };
    let receipt = accepted(trading.orca_submit(&strategy_sdk::to_payload(&open)).await);
    let Receipt::Opened {
        position,
        lower_price,
        upper_price,
    } = receipt
    else {
        panic!("an open opens: {receipt:?}");
    };
    assert!(lower_price <= decimal("140") && upper_price >= decimal("159.99"));
    let opened = state(&trading).await;
    assert_eq!(opened.positions.len(), 1);
    let held = &opened.positions[0];
    assert_eq!(held.position, position);
    assert!(held.in_range && held.liquidity != "0");
    assert!(
        opened.value > decimal("3500") && opened.value <= decimal("4001"),
        "{}",
        opened.value
    );
    assert!(opened.exit_value <= opened.value && opened.exit_value > decimal("3400"));
    assert!(!opened.outside);
    assert_eq!(opened.max_positions, 1);
    // A second band is refused: the station has one slot.
    let refused = trading.orca_submit(&strategy_sdk::to_payload(&open)).await;
    assert!(matches!(refused, Outcome::Refused(why) if why.contains("most the driver allows")));
    // The key paid the position's rent and the tick arrays it made, billed
    // to the tank as its other sends are.
    assert!(spent.load(std::sync::atomic::Ordering::SeqCst) > 0);

    // The keeper values the vault through the driver.
    setup::price_follows_pool(&client, now().await)
        .await
        .unwrap();
    let started = Instant::now();
    let valued = loop {
        if let Some(landed) = keep(&client, &platform, &v).await {
            break landed;
        }
        assert!(started.elapsed() < PATIENCE, "no valuation went through");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert!(
        valued
            .record
            .instructions
            .iter()
            .any(|call| call.accounts.contains(&venue::POOLS[0].address)),
        "the valuation read the pool"
    );

    // The depositor asks for everything back: more than the vault's cash,
    // so a claim is booked and served by closing its share at Orca.
    let shares = platform
        .shares(&client, &v.vault, &viewer.pubkey())
        .await
        .unwrap();
    let redeem =
        platform.ix_request_redeem(&v.vault, &viewer.pubkey(), shares, &viewer.pubkey(), 2);
    client.send(vec![redeem], &[viewer]).await.unwrap();
    let request = platform
        .vault(&client, &v.vault)
        .await
        .unwrap()
        .request_count;
    let dollars_before = client
        .token_balance(&platform.dollars(&viewer.pubkey()))
        .await
        .unwrap();
    let started = Instant::now();
    // Half of it first, as the keeper serves a claim that can't be served
    // whole: that half paid, the rest left booked.
    let mut halved = false;
    loop {
        setup::price_follows_pool(&client, now().await)
            .await
            .unwrap();
        keep(&client, &platform, &v).await;
        if let Some(claim) = platform.claim(&client, &v.vault, request).await.unwrap() {
            if claim.state == vault::state::ClaimState::Paid {
                break;
            }
            let ix = if halved {
                platform
                    .ix_settle_claim(&client, &v, &keeper.pubkey(), request)
                    .await
                    .unwrap()
            } else {
                platform
                    .ix_settle_claim_part(&client, &v, &keeper.pubkey(), request, claim.shares / 2)
                    .await
                    .unwrap()
            };
            if client.send(vec![ix], &[keeper]).await.is_ok() && !halved {
                let after = platform
                    .claim(&client, &v.vault, request)
                    .await
                    .unwrap()
                    .unwrap();
                if after.shares < claim.shares {
                    assert_eq!(after.shares, claim.shares - claim.shares / 2);
                    assert_eq!(after.state, vault::state::ClaimState::Pending);
                    assert!(after.paid > 0, "the half was paid");
                    halved = true;
                }
            }
        }
        assert!(started.elapsed() < PATIENCE, "the claim was never paid");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(halved, "the claim was served in two parts");
    let dollars_after = client
        .token_balance(&platform.dollars(&viewer.pubkey()))
        .await
        .unwrap();
    assert!(
        dollars_after > dollars_before + DEPOSIT * 9 / 10,
        "the depositor was paid: {dollars_before} to {dollars_after}"
    );
    let after = state(&trading).await;
    let left: u128 = after.positions[0].liquidity.parse().unwrap();
    let had: u128 = held.liquidity.parse().unwrap();
    assert!(
        left < had / 2,
        "the leaver's share of the band was closed: {had} to {left}"
    );

    // The market operator brings the pool to a price, as the devnet's market
    // keeps it with the tape locally and with Pyth on devnet.
    let operator = role(Role::Operator);
    let admin = role(Role::Admin);
    let target = 153 * setup::PRICE_ONE;
    let reached = setup::steer(&client, &platform, admin, operator, target)
        .await
        .unwrap();
    assert!(
        reached.abs_diff(target) * 10_000 <= target,
        "the pool is within a basis point of $153: {reached}"
    );
    let unmoved = setup::steer(&client, &platform, admin, operator, target)
        .await
        .unwrap();
    assert_eq!(unmoved, reached, "a pool already there is left alone");
    // A centered position still needs a successful steer. Remove only the
    // test operator's free SOL so a normal sale cannot be funded, keeping
    // the seed and pool unchanged. Failed steering must reach its caller.
    let pool = setup::pool_now(&client).await.unwrap();
    let target = venue::price_of_sqrt(pool.state.sqrt_price) * 9_899 / 10_000;
    let seed = setup::seed_position(&client, operator)
        .await
        .unwrap()
        .unwrap();
    assert!(!setup::needs_recentring(&seed, target));
    let roots = venue::band_roots(target, 100);
    assert!(pool.state.sqrt_price > roots.0.max(roots.1));
    let native = client
        .rpc
        .account(&operator.pubkey())
        .await
        .unwrap()
        .unwrap();
    let wrapped_at = venue::wsol_of(&operator.pubkey());
    let wrapped = client.rpc.account(&wrapped_at).await.unwrap().unwrap();
    let amount = u64::from_le_bytes(wrapped.data[64..72].try_into().unwrap());
    let mut empty_native = native.clone();
    empty_native.lamports = setup::OPERATOR_KEEPS;
    let mut empty_wrapped = wrapped.clone();
    empty_wrapped.data[64..72].copy_from_slice(&0u64.to_le_bytes());
    empty_wrapped.lamports -= amount;
    client
        .rpc
        .set_account(&operator.pubkey(), &empty_native)
        .await
        .unwrap();
    client
        .rpc
        .set_account(&wrapped_at, &empty_wrapped)
        .await
        .unwrap();
    let failed = setup::keep_seed_at(&client, &platform, admin, operator, target, None).await;
    client
        .rpc
        .set_account(&operator.pubkey(), &native)
        .await
        .unwrap();
    client.rpc.set_account(&wrapped_at, &wrapped).await.unwrap();
    assert!(
        failed.is_err(),
        "failed steering was reported as successful upkeep"
    );
    assert!(format!("{:#}", failed.unwrap_err()).contains("nothing to move"));
}
