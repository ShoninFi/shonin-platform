//! The local Solana cluster: the platform's programs deployed to Surfpool and
//! set up as a development cluster is, then its clock and its market driven
//! as the EVM chains' are.
//!
//! The programs go on through the real loader (`solana program deploy`), so
//! what runs locally is what a cluster runs. The development checker and the
//! stand-in driver are deployed final, as every checker and driver must be,
//! before the registry is set up. The platform's own programs stay
//! upgradeable by the deployer locally, which is the key that sets each of
//! them up.
//!
//! On testnets the same programs are deployed by `task
//! contracts:solana:deploy`, at keys of the team's mnemonic
//! ([`crate::devnet_keys`]); nothing here touches a public cluster.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chain::solana::keys::{self, RoleKeys};
use chain::solana::platform::{Platform, image_digest, program_data_address, project_id};
use chain::solana::programs::{
    common, local_driver, local_venue, prices, registry, station, vault,
};
use chain::solana::{Pubkey, Signer, SolanaClient, name};
use chain::{ChainKey, Role, SolanaDeployments, SolanaRoles};

use crate::market::{Tape, half_spread};

pub use chain::solana::keys::{DEPLOYER, PROGRAM, keypair_from_mnemonic, testnet_mnemonic};

/// The key of the development mnemonic the dollar's mint is at, so the
/// local dollar has the same address every run.
const MINT_KEY: u32 = 2;

/// How long an image or a driver waits between being proposed and becoming
/// usable: two days, under a minute on the fast clock.
const TIMELOCK: u64 = 2 * DAY;
const BLESSING_TTL: u64 = 72 * HOUR;
/// What publishing costs: a flat dollar.
const PUBLISH_FEE: u64 = 1_000_000;
/// How long before a new price list takes effect: thirty days.
const PRICE_NOTICE: u64 = 30 * DAY;
const MARGIN_BPS: u64 = 1_000;
/// What every account is given, in dollars and in coin.
const STARTING_DOLLARS: u64 = 1_000_000_000_000;
const STARTING_LAMPORTS: u64 = 1_000 * LAMPORTS_PER_SOL;
/// The venue's own reserves, as a real venue's other traders would be, so a
/// gain can always be withdrawn.
const VENUE_RESERVES: u64 = 10_000_000_000_000;
const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

pub const SPOT: &str = crate::deploy::SPOT;
pub const PERP: &str = crate::deploy::PERP;
const STARTING_PRICE: u64 = crate::deploy::STARTING_PRICE;

/// Every program a development cluster runs, by its library name, and
/// whether it is deployed final.
const PROGRAMS: [(&str, bool); 9] = [
    ("shonin_dev_verifier", true),
    ("shonin_local_driver", true),
    ("shonin_registry", false),
    ("shonin_publishing", false),
    ("shonin_prices", false),
    ("shonin_profiles", false),
    ("shonin_station", false),
    ("shonin_vault", false),
    ("shonin_local_venue", false),
];

/// Whether `program` is deployed final, on any cluster.
pub fn deployed_final(program: &str) -> bool {
    PROGRAMS.contains(&(program, true))
}

/// The terms every local vault is created with: the EVM factory's.
pub fn terms() -> vault::state::Terms {
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
            fallback_after: DAY,
            // Many local blocks pass between an executor's signings.
            usage_freshness: DAY,
            tank_escape_delay: DAY,
            floor_delay: DAY,
            halt_grace: 10 * MINUTE,
            freeze_after: 7 * DAY,
            wind_down: 7 * DAY,
        },
    }
}

/// Deploy and set the platform up if this cluster doesn't have it, and
/// return what is there.
pub async fn ensure(
    client: &SolanaClient,
    key: &ChainKey,
    rpc: &str,
    programs: &Path,
    file: &Path,
) -> Result<SolanaDeployments> {
    if let Ok(text) = std::fs::read_to_string(file)
        && let Ok(existing) = SolanaDeployments::from_json(&text)
        && existing.roles.is_some()
        && client.check_deployments(&existing).await.is_ok()
        && client
            .account(&vault::state::factory_address())
            .await?
            .is_some()
    {
        tracing::info!(file = %file.display(), "the programs are already on this cluster");
        return Ok(existing);
    }
    let keys = client.keys()?;
    let deployed_slot = client.rpc.slot().await?;
    let deployer_file = file
        .parent()
        .and_then(Path::parent)
        .unwrap_or(Path::new("."))
        .join("solana-deployer.json");
    keys::write_keypair(&keys.deployer, &deployer_file)?;

    fund(client, keys).await?;
    let ids = deploy_programs(client, rpc, programs, &deployer_file).await?;
    let (platform, checker) = set_up(client, keys).await?;

    let deployments = SolanaDeployments {
        chain_key: key.to_string(),
        cluster: client.cluster.clone(),
        rpc_url: rpc.to_string(),
        genesis_hash: client.genesis_hash.clone(),
        deployed_slot,
        programs: ids,
        mint: Some(platform.mint.to_string()),
        token_program: Some(chain::solana::programs::anchor_spl::token::ID.to_string()),
        checker: Some(checker.to_string()),
        roles: Some(roles(keys)),
    };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    std::fs::write(file, deployments.to_json())
        .with_context(|| format!("writing {}", file.display()))?;
    tracing::info!(file = %file.display(), "the Solana programs are written down");
    Ok(deployments)
}

fn roles(keys: &RoleKeys) -> SolanaRoles {
    let at = |role| keys.address(role).to_string();
    SolanaRoles {
        deployer: keys.deployer.pubkey().to_string(),
        admin: at(Role::Admin),
        platform: at(Role::Platform),
        attestor: at(Role::Attestor),
        builder: at(Role::Builder),
        reviewer: at(Role::Reviewer),
        executor: at(Role::Executor),
        keeper: at(Role::Keeper),
        operator: at(Role::Operator),
        author: at(Role::Author),
        viewer: at(Role::Viewer),
    }
}

/// Coin for the deployer and every role, from the cluster's faucet.
async fn fund(client: &SolanaClient, keys: &RoleKeys) -> Result<()> {
    let mut everyone = vec![keys.deployer.pubkey()];
    everyone.extend(ROLES.iter().map(|role| keys.address(*role)));
    for who in everyone {
        if client.lamports(&who).await? < STARTING_LAMPORTS / 2 {
            client.airdrop(&who, STARTING_LAMPORTS).await?;
        }
    }
    Ok(())
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

/// Each program through the loader, all at once, at the address of its
/// keypair, which is the address this build compiled in. A program already
/// there is left as it is.
async fn deploy_programs(
    client: &SolanaClient,
    rpc: &str,
    dir: &Path,
    deployer: &Path,
) -> Result<std::collections::BTreeMap<String, String>> {
    let compiled = chain::solana::programs::compiled();
    let mut running = tokio::task::JoinSet::new();
    let mut ids = std::collections::BTreeMap::new();
    for (program, fixed) in PROGRAMS {
        let so = dir.join(format!("{program}.so"));
        let keypair = dir.join(format!("{program}-keypair.json"));
        if !so.exists() || !keypair.exists() {
            bail!(
                "{} is missing: build the Solana programs first (`task contracts:solana:build`)",
                so.display()
            );
        }
        let id = keys::read_keypair(&keypair)?.pubkey();
        let expected = compiled
            .iter()
            .find(|(name, _)| *name == program)
            .map(|(_, id)| *id)
            .ok_or_else(|| anyhow!("{program} is not one this build knows"))?;
        if id != expected {
            bail!(
                "{program}'s keypair is at {id}, and this build expects {expected}: \
                 rebuild after the programs' keypairs changed"
            );
        }
        ids.insert(program.to_string(), id.to_string());
        if client
            .account(&id)
            .await?
            .is_some_and(|account| account.executable)
        {
            tracing::info!(program, %id, "already deployed");
            continue;
        }
        let mut command = tokio::process::Command::new("solana");
        command
            .args([
                "program",
                "deploy",
                "--use-rpc",
                "--commitment",
                "confirmed",
            ])
            .arg("--url")
            .arg(rpc)
            .arg("--keypair")
            .arg(deployer)
            .arg("--program-id")
            .arg(&keypair)
            .arg(&so);
        if fixed {
            command.arg("--final");
        }
        command.kill_on_drop(true);
        running.spawn(async move {
            let output = command
                .output()
                .await
                .context("running `solana program deploy`: is the Solana CLI installed?")?;
            if !output.status.success() {
                bail!(
                    "deploying {program} failed:\n{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            tracing::info!(program, %id, fixed, "deployed");
            Ok(())
        });
    }
    while let Some(done) = running.join_next().await {
        done??;
    }
    Ok(ids)
}

/// Everything a development cluster has before anyone publishes: the
/// dollar, the development checker, every program set up, the venue's
/// markets, the images, project and driver proposed, and money handed out.
async fn set_up(client: &SolanaClient, keys: &RoleKeys) -> Result<(Platform, Pubkey)> {
    let deployer = &keys.deployer;
    let admin = keys.get(Role::Admin);
    let operator = keys.get(Role::Operator);
    let mint = keys::keypair_from_mnemonic(keys::DEVELOPMENT_MNEMONIC, MINT_KEY)?;
    let cluster = client.cluster.clone();
    let (create_checker, checker) = Platform::ix_create_checker(
        &deployer.pubkey(),
        keys.address(Role::Attestor),
        common::digest::keccak(&[b"shonin-dev-checker"]),
        &cluster,
    );
    let platform = Platform {
        cluster_name: cluster.clone(),
        cluster: client.cluster_value(),
        mint: mint.pubkey(),
        admin: admin.pubkey(),
        platform: keys.address(Role::Platform),
        operator: operator.pubkey(),
        checker: Some(checker),
    };

    // The dollar, minted by the admin, as anyone may mint the EVM's local one.
    if client.account(&mint.pubkey()).await?.is_none() {
        let rent = client
            .rpc
            .rent_exempt_minimum(Platform::rent_for_mint())
            .await?;
        let ixs =
            Platform::ix_create_mint(&deployer.pubkey(), &mint.pubkey(), &admin.pubkey(), rent)?;
        client
            .send(ixs, &[deployer, &mint])
            .await
            .context("creating the dollar")?;
    }

    // The checker must be final before the registry trusts it, which the
    // deploy made it.
    if client.account(&checker).await?.is_none() {
        client
            .send(vec![create_checker], &[deployer])
            .await
            .context("creating the development checker")?;
    }
    if client
        .account(&registry::read::config_address())
        .await?
        .is_none()
    {
        let ix = Platform::ix_initialize_registry(
            &deployer.pubkey(),
            registry::InitializeArgs {
                admin: admin.pubkey(),
                cluster_name: cluster.clone(),
                timelock: TIMELOCK,
                min_timelock: 0,
                blessing_ttl: BLESSING_TTL,
            },
            checker,
        );
        client
            .send(vec![ix], &[deployer])
            .await
            .context("setting the registry up")?;
    }
    if client
        .account(&chain::solana::platform::publishing_config())
        .await?
        .is_none()
    {
        let ix = platform.ix_initialize_publishing(&deployer.pubkey(), PUBLISH_FEE);
        client
            .send(vec![ix], &[deployer])
            .await
            .context("setting publishing up")?;
    }
    if client
        .account(&prices::read::prices_address())
        .await?
        .is_none()
    {
        let ix =
            platform.ix_initialize_prices(&deployer.pubkey(), PRICE_NOTICE, published_prices());
        client
            .send(vec![ix], &[deployer])
            .await
            .context("setting the prices up")?;
    }
    if client.account(&station::config_address()).await?.is_none() {
        let ix = platform.ix_initialize_stations(&deployer.pubkey());
        client
            .send(vec![ix], &[deployer])
            .await
            .context("setting the stations up")?;
    }
    if client
        .account(&local_venue::venue_address())
        .await?
        .is_none()
    {
        let ix = platform.ix_initialize_venue(&deployer.pubkey(), MARGIN_BPS);
        client
            .send(vec![ix], &[deployer])
            .await
            .context("setting the stand-in venue up")?;
    }
    if client
        .account(&vault::state::factory_address())
        .await?
        .is_none()
    {
        let ix = platform.ix_initialize_vaults(&deployer.pubkey(), terms());
        client
            .send(vec![ix], &[deployer])
            .await
            .context("setting the vault factory up")?;
    }

    // The venue's markets, and its reserves.
    for (market, is_perp) in [(SPOT, false), (PERP, true)] {
        if client
            .account(&local_venue::market_address(&name(market)))
            .await?
            .is_none()
        {
            let ix = platform.ix_list_market(name(market), is_perp, STARTING_PRICE);
            client.send(vec![ix], &[operator]).await?;
        }
    }
    client
        .send(
            vec![platform.ix_mint_into(
                &admin.pubkey(),
                &local_venue::vault_address(),
                VENUE_RESERVES,
            )?],
            &[admin],
        )
        .await
        .context("the venue's reserves")?;
    tracing::info!("the stand-in venue has listed its markets");

    // The images, the project and the stand-in driver, behind the timelock.
    let mut proposals = Vec::new();
    for (service, kind) in [
        ("executor", registry::state::ImageKind::Executor),
        ("builder", registry::state::ImageKind::Builder),
        ("reviewer", registry::state::ImageKind::Reviewer),
        ("unwinder", registry::state::ImageKind::Unwinder),
    ] {
        let digest = image_digest(service);
        if client
            .account(&registry::read::image_address(&digest))
            .await?
            .is_none()
        {
            proposals.push(platform.ix_propose_image(digest, kind));
        }
    }
    if client
        .account(&registry::read::project_address(&project_id()))
        .await?
        .is_none()
    {
        proposals.push(platform.ix_propose_project(project_id()));
    }
    if !proposals.is_empty() {
        client
            .send(proposals, &[admin])
            .await
            .context("proposing the images")?;
    }
    let driver_slot = registry::read::approval_address(
        registry::state::ApprovalKind::Driver,
        &local_driver::NAME,
    );
    if client.account(&driver_slot).await?.is_none() {
        let ix = platform
            .ix_propose(
                client,
                registry::state::ApprovalKind::Driver,
                local_driver::NAME,
                local_driver::ID,
            )
            .await?;
        client
            .send(vec![ix], &[admin])
            .await
            .context("proposing the stand-in driver")?;
    }
    debug_assert_ne!(program_data_address(&local_driver::ID), Pubkey::default());
    tracing::info!("the images and the stand-in driver are proposed; the timelock is running");

    // Money: every role's dollar account, and dollars for those who spend.
    let mut open = Vec::new();
    for role in ROLES {
        open.push(platform.ix_open_dollars(&deployer.pubkey(), &keys.address(role)));
    }
    client.send(open, &[deployer]).await?;
    let mut minting = Vec::new();
    for role in [Role::Author, Role::Viewer, Role::Keeper, Role::Admin] {
        minting.push(platform.ix_mint_to(
            &admin.pubkey(),
            &keys.address(role),
            STARTING_DOLLARS,
        )?);
    }
    client.send(minting, &[admin]).await?;
    tracing::info!("the accounts are funded");
    Ok((platform, checker))
}

/// The published prices, in dollars scaled by 1e18: a core-hour, a
/// GiB-hour, a GB of egress, a second of inference.
pub fn published_prices() -> prices::UnitPrices {
    prices::UnitPrices {
        vcpu_hour: 150_000_000_000_000_000,
        gib_hour: 10_000_000_000_000_000,
        gb_egress: 150_000_000_000_000_000,
        inference_second: 150_000_000_000_000_000 / 3600,
    }
}

/// The clock and the market, as on the EVM chains: every second the cluster's
/// clock jumps `seconds_per_block` (a multiple of the ten-second slot, so it
/// always lands on a slot boundary, where a valuation counts) and the venue
/// is given new prices and the funding accrued since.
pub async fn run_market(
    client: &SolanaClient,
    deployments: &SolanaDeployments,
    seconds_per_block: u64,
) -> Result<()> {
    const SLOT: u64 = 10;
    let platform = Platform::from_deployments(deployments)?;
    let operator = client.role(Role::Operator)?;
    let step = seconds_per_block.div_ceil(SLOT).max(1) * SLOT;
    let mut tape = Tape::new(step);
    let now = client.now().await?;
    let mut at = now - now % SLOT + SLOT;
    tracing::info!(
        seconds_per_block = step,
        "the Solana clock is running; the venue is quoting"
    );
    loop {
        // The clock only moves forward, and runs on at real pace between
        // jumps, so the next mark is always ahead of it.
        let now = client.now().await?;
        while at <= now {
            at += step;
        }
        if let Err(error) = client.rpc.time_travel(at).await {
            tracing::warn!(error = format!("{error:#}"), "the clock would not move");
        }
        let tick = tape.step();
        let ix = platform.ix_post(
            vec![quote(SPOT, tick.spot), quote(PERP, tick.perp)],
            vec![local_venue::Funding {
                market: name(PERP),
                rate_per_day: tick.rate_per_day as i128,
                paid_per_unit: tick.accrued as i128,
            }],
        );
        // Not waited for: a block's quote landing late holds up no clock.
        let sending = client.clone();
        let operator = operator.insecure_clone();
        tokio::spawn(async move {
            if let Err(error) = sending.send(vec![ix], &[&operator]).await {
                tracing::warn!(
                    error = format!("{error:#}"),
                    "the venue would not take the quote"
                );
            }
        });
        tokio::time::sleep(Duration::from_secs(1)).await;
        at += step;
    }
}

fn quote(market: &str, price: f64) -> local_venue::Quote {
    let mark = price.max(1.0) as u64;
    let half = half_spread(price) as u64;
    local_venue::Quote {
        market: name(market),
        mark,
        oracle: mark,
        bid: mark.saturating_sub(half),
        ask: mark + half,
    }
}
