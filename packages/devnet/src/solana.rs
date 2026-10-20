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
//! On devnet the same programs are deployed by `task
//! contracts:solana:deploy NETWORK=testnet`, at keys of the team's mnemonic
//! ([`crate::devnet_keys`]), and [`set_up_public`] sets them up from there
//! as a development cluster is set up, with a public cluster's choices
//! ([`Settings::testnet`]): the roles funded by transfer from the deployer,
//! Circle's devnet USDC as the dollar, with no stand-in venue or driver and
//! nothing minted, as on the EVM testnets, no timelock, the operator's real
//! project, and no image at all until a real digest is proposed
//! (`chains:image`). Nothing there is driven: devnet keeps its own time,
//! and Orca's pool is kept at Pyth's price by `testnet-market`.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chain::solana::keys::{self, RoleKeys};
use chain::solana::platform::{Platform, image_digest, program_data_address, project_id};
use chain::solana::programs::{
    common, local_driver, local_venue, orca_driver, prices, registry, station, vault,
};
use chain::solana::{Keypair, Pubkey, Signer, SolanaClient, name};
use chain::{ChainKey, Role, SolanaDeployments, SolanaRoles};
use registry::state::ImageKind;

use crate::market::{Tape, half_spread};

pub use chain::solana::keys::{DEPLOYER, PROGRAM, keypair_from_mnemonic, testnet_mnemonic};

/// The key of the development mnemonic the dollar's mint is at, so the
/// local dollar has the same address every run.
const MINT_KEY: u32 = 2;

/// How long an image or a driver waits between being proposed and becoming
/// usable on a local cluster: two days, under a minute on the fast clock.
/// None on devnet, as on the EVM testnets: nobody else's money is at stake
/// there, and its clock runs in real time.
const TIMELOCK: u64 = 2 * DAY;
const BLESSING_TTL: u64 = 72 * HOUR;
/// What publishing costs: a flat dollar.
const PUBLISH_FEE: u64 = 1_000_000;
/// How long before a new price list takes effect on a local cluster: thirty
/// days. None on devnet, as on the EVM testnets.
const PRICE_NOTICE: u64 = 30 * DAY;
const MARGIN_BPS: u64 = 1_000;
/// What every account is given, in dollars and in coin.
const STARTING_DOLLARS: u64 = 1_000_000_000_000;
const STARTING_LAMPORTS: u64 = 1_000 * LAMPORTS_PER_SOL;
/// The venue's own reserves, as a real venue's other traders would be, so a
/// gain can always be withdrawn.
pub(crate) const VENUE_RESERVES: u64 = 10_000_000_000_000;
const LAMPORTS_PER_SOL: u64 = 1_000_000_000;

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

pub const SPOT: &str = crate::deploy::SPOT;
pub const PERP: &str = crate::deploy::PERP;
const STARTING_PRICE: u64 = crate::deploy::STARTING_PRICE;

/// Every program a development cluster runs, by its library name, and
/// whether it is deployed final.
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

/// Whether `program` is deployed final, on any cluster.
pub fn deployed_final(program: &str) -> bool {
    PROGRAMS.contains(&(program, true))
}

/// Whether the program at `id` is final, and if not, who may still replace
/// it, from its program-data account; `None` if no program is there.
async fn upgrade_authority(client: &SolanaClient, id: &Pubkey) -> Result<Option<Option<Pubkey>>> {
    if !client
        .account(id)
        .await?
        .is_some_and(|account| account.executable)
    {
        return Ok(None);
    }
    // The upgradeable loader's program-data header: a tag of 3, the slot
    // it was deployed in, then an optional authority.
    let data = client
        .account(&program_data_address(id))
        .await?
        .map(|account| account.data)
        .unwrap_or_default();
    match (data.get(..4), data.get(12)) {
        (Some([3, 0, 0, 0]), Some(0)) => Ok(Some(None)),
        (Some([3, 0, 0, 0]), Some(1)) => {
            let authority = data
                .get(13..45)
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                .ok_or_else(|| anyhow!("{id}'s program data is cut short"))?;
            Ok(Some(Some(Pubkey::new_from_array(authority))))
        }
        _ => bail!("{id} is not a program of the upgradeable loader this can read"),
    }
}

/// Circle's USDC on Solana devnet: a public cluster's dollar, which only
/// Circle's faucet gives out.
pub const CIRCLE_DEVNET_USDC: Pubkey =
    Pubkey::from_str_const("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU");

/// The dollar a cluster is set up with.
pub enum Dollar {
    /// The platform's own mint at this key, minted by the admin: a
    /// development cluster's, beside the stand-in venue and driver.
    Own(Keypair),
    /// A dollar nobody here mints (Circle's USDC): a public cluster's, with
    /// no stand-in venue or driver.
    Public(Pubkey),
}

impl Dollar {
    pub fn pubkey(&self) -> Pubkey {
        match self {
            Self::Own(mint) => mint.pubkey(),
            Self::Public(mint) => *mint,
        }
    }

    /// Whether the stand-ins (the venue, its markets and reserves, the
    /// stand-in driver, and minted money) go on the cluster.
    pub fn stand_ins(&self) -> bool {
        matches!(self, Self::Own(_))
    }
}

/// What sets a cluster up: everything a development cluster and a public one
/// do differently. The rest of the set-up is the same on both.
pub struct Settings {
    /// The dollar.
    pub mint: Dollar,
    /// The registry's timelock and its floor.
    pub timelock: u64,
    pub min_timelock: u64,
    pub blessing_ttl: u64,
    /// How long a new price list waits before it applies.
    pub price_notice: u64,
    /// The terms every vault is created with.
    pub terms: vault::state::Terms,
    /// The images proposed with the registry. A development cluster's are
    /// its placeholders; a public cluster's are none: a real image's digest
    /// is proposed once it is built (`chains:image`).
    pub images: Vec<([u8; 32], ImageKind)>,
    /// The operator's project, as the registry holds it.
    pub project: [u8; 32],
    pub funding: Funding,
}

/// How the roles are given coin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Funding {
    /// From the cluster's faucet: a development cluster's.
    Airdrop,
    /// By transfer from the deployer, the Solana funder, which keeps
    /// `reserve` lamports for itself: each role topped up to its amount, and
    /// left alone once it holds it.
    Transfer {
        reserve: u64,
        roles: Vec<(Role, u64)>,
    },
}

impl Settings {
    /// A local cluster's: the development mnemonic's mint, a two-day
    /// timelock on the fast clock, placeholder images and project, and the
    /// faucet.
    pub fn local() -> Result<Self> {
        Ok(Self {
            mint: Dollar::Own(keys::keypair_from_mnemonic(
                keys::DEVELOPMENT_MNEMONIC,
                MINT_KEY,
            )?),
            timelock: TIMELOCK,
            min_timelock: 0,
            blessing_ttl: BLESSING_TTL,
            price_notice: PRICE_NOTICE,
            terms: terms(),
            images: ["executor", "builder", "reviewer", "unwinder"]
                .into_iter()
                .zip([
                    ImageKind::Executor,
                    ImageKind::Builder,
                    ImageKind::Reviewer,
                    ImageKind::Unwinder,
                ])
                .map(|(service, kind)| (image_digest(service), kind))
                .collect(),
            project: project_id(),
            funding: Funding::Airdrop,
        })
    }

    /// Devnet's, as the EVM testnets are set up: Circle's devnet USDC as
    /// the dollar and no stand-ins, the team's mnemonic `phrase` (never the
    /// published development one, whose keys anyone holds), no timelock,
    /// the testnet terms, no image until a real digest is proposed, the
    /// operator's real Google Cloud `project`, and every role funded by
    /// transfer from the deployer as the funding file gives `cluster`'s
    /// amounts.
    pub fn testnet(
        phrase: &str,
        project: &str,
        funding: &crate::fund::Funding,
        cluster: &ChainKey,
    ) -> Result<Self> {
        if keys::keypair_from_mnemonic(phrase, DEPLOYER)?.pubkey()
            == keys::keypair_from_mnemonic(keys::DEVELOPMENT_MNEMONIC, DEPLOYER)?.pubkey()
        {
            bail!(
                "{} is the published development phrase, whose keys are anyone's: a public cluster is set up from the team's own",
                keys::TESTNET_SOLANA_MNEMONIC
            );
        }
        let crate::fund::SolanaAmounts { reserve, roles } =
            funding.solana(cluster)?.ok_or_else(|| {
                anyhow!(
                    "the funding file has no [testnet.\"{cluster}\"]: what each role is given there"
                )
            })?;
        Ok(Self {
            mint: Dollar::Public(CIRCLE_DEVNET_USDC),
            timelock: 0,
            min_timelock: 0,
            blessing_ttl: BLESSING_TTL,
            price_notice: 0,
            terms: testnet_terms(),
            images: Vec::new(),
            project: crate::admin::project_id(checked_project(project)?).0,
            funding: Funding::Transfer { reserve, roles },
        })
    }
}

/// Whether the cluster's admin mints the platform's dollar: the stand-in
/// dollar's mint authority is the admin; Circle's USDC's is not.
pub async fn admin_mints(client: &SolanaClient, platform: &Platform) -> Result<bool> {
    let data = client
        .account(&platform.mint)
        .await?
        .map(|account| account.data)
        .unwrap_or_default();
    Ok(mint_authority(&data) == Some(platform.admin))
}

/// A token mint's authority, from its account's data: an optional key
/// first, as the token program lays it out.
fn mint_authority(data: &[u8]) -> Option<Pubkey> {
    match data.get(..4)? {
        [1, 0, 0, 0] => <[u8; 32]>::try_from(data.get(4..36)?)
            .ok()
            .map(Pubkey::new_from_array),
        _ => None,
    }
}

/// A Google Cloud project id as the console makes one: 6 to 30 lowercase
/// letters, digits and hyphens, starting with a letter and not ending with a
/// hyphen. Anything else is a typo, and its hash would allow nothing.
fn checked_project(project: &str) -> Result<&str> {
    let project = project.trim();
    let shaped = (6..=30).contains(&project.len())
        && project.starts_with(|c: char| c.is_ascii_lowercase())
        && !project.ends_with('-')
        && project
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !shaped {
        bail!("{project:?} is not a Google Cloud project id");
    }
    Ok(project)
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

/// The terms every devnet vault is created with: the EVM testnets' (their
/// fees, minimums and timers; the executor signs its usage totals every few
/// minutes, so a total is fresh for ten).
pub fn testnet_terms() -> vault::state::Terms {
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
        minimum_tank: 10_000_000,
        dead_assets: 10_000,
        timers: Timers {
            slot_interval: 10,
            slot_lead: 5,
            slot_window: 3,
            fallback_after: DAY,
            usage_freshness: 10 * MINUTE,
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
    orca: &Path,
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

    let settings = Settings::local()?;
    fund(client, keys, &settings.funding).await?;
    let ids = deploy_programs(client, rpc, programs, &deployer_file).await?;
    let (platform, checker) = set_up(client, keys, &settings).await?;
    crate::solana_orca::set_up_local(
        client,
        &platform,
        keys.get(Role::Admin),
        keys.get(Role::Operator),
        &keys.deployer,
        orca,
    )
    .await
    .context("setting Orca up")?;

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

/// Set a public cluster up from the programs already deployed to it, and
/// write its deployment file (`deployments/solana-devnet.json`): every role
/// funded, then everything a development cluster has before anyone
/// publishes, with `settings`' choices. Safe to run again: every step looks
/// at the cluster first, a role holding its amount is given nothing, and the
/// file keeps the slot of the first run. Fails, once the rest is done, if
/// the deployer couldn't cover a role past its reserve.
pub async fn set_up_public(
    client: &SolanaClient,
    key: &ChainKey,
    public_rpc: &str,
    file: &Path,
    settings: &Settings,
) -> Result<SolanaDeployments> {
    let keys = client.keys()?;
    let ids = programs_in_place(client).await?;
    // A public cluster's dollar is one nobody here mints: until it is there,
    // nothing is sent, not even the roles' coin.
    if let Dollar::Public(mint) = &settings.mint
        && client.account(mint).await?.is_none()
    {
        bail!("the dollar's mint {mint} is not on {}", client.key);
    }
    // The slot the first run began in is where an indexer starts reading.
    let earlier = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| SolanaDeployments::from_json(&text).ok());
    let deployed_slot = match earlier {
        Some(earlier)
            if earlier.deployed_slot != 0 && earlier.genesis_hash == client.genesis_hash =>
        {
            earlier.deployed_slot
        }
        _ => client.rpc.slot().await?,
    };

    let short = fund(client, keys, &settings.funding).await?;
    // The admin and the operator sign the set-up: nothing goes on without
    // them.
    if let Some(role) = short
        .iter()
        .find(|role| matches!(role, Role::Admin | Role::Operator))
    {
        bail!(
            "the deployer can't give the {} its amount past its reserve: fund the deployer ({}) and run this again",
            role.label(),
            keys.deployer.pubkey()
        );
    }
    let (platform, checker) = set_up(client, keys, settings).await?;
    // Orca's pool is the testnet market's to seed and keep at Pyth's price
    // (`task chains:testnet-market-setup STEPS=pools`), with its own key.
    if client
        .account(&chain::solana::orca::POOLS[0].address)
        .await?
        .is_none()
    {
        tracing::warn!(
            pool = %chain::solana::orca::POOLS[0].address,
            "Orca's pool this build lists is not on the cluster yet: testnet-market-setup creates and seeds it"
        );
    }

    let deployments = SolanaDeployments {
        chain_key: key.to_string(),
        cluster: client.cluster.clone(),
        // The chain list's endpoint, never an override, which may carry a
        // key: this file goes into images.
        rpc_url: public_rpc.to_string(),
        genesis_hash: client.genesis_hash.clone(),
        deployed_slot,
        programs: ids,
        mint: Some(platform.mint.to_string()),
        token_program: Some(chain::solana::programs::anchor_spl::token::ID.to_string()),
        checker: Some(checker.to_string()),
        roles: Some(roles(keys)),
    };
    client
        .check_deployments(&deployments)
        .await
        .context("the deployment just set up does not check out")?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    std::fs::write(file, deployments.to_json())
        .with_context(|| format!("writing {}", file.display()))?;
    tracing::info!(file = %file.display(), "the cluster is set up and written down");
    if !short.is_empty() {
        let named: Vec<&str> = short.iter().map(|role| role.label()).collect();
        bail!(
            "set up, but the deployer ({}) couldn't give {} their amounts past its reserve: fund it and run this again",
            keys.deployer.pubkey(),
            named.join(", ")
        );
    }
    Ok(deployments)
}

/// Every program this build speaks to, deployed at the id compiled in, and
/// final where it must be (the development checker and the stand-in
/// driver, which the registry checks for itself); their ids by name.
async fn programs_in_place(
    client: &SolanaClient,
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut ids = std::collections::BTreeMap::new();
    for (program, id) in chain::solana::programs::compiled() {
        match upgrade_authority(client, &id).await? {
            None => bail!(
                "{program} is not deployed at {id} on {}: deploy the programs first (task contracts:solana:deploy NETWORK=testnet)",
                client.key
            ),
            Some(Some(authority)) if deployed_final(program) => bail!(
                "{program} at {id} can still be replaced (by {authority}): a checker or a driver must be final"
            ),
            Some(_) => {}
        }
        ids.insert(program.to_string(), id.to_string());
    }
    Ok(ids)
}

/// Coin for the deployer and every role: from the faucet on a development
/// cluster, by transfer from the deployer on a public one. The roles the
/// deployer couldn't cover past its reserve are returned: none is given part
/// of its amount.
async fn fund(client: &SolanaClient, keys: &RoleKeys, funding: &Funding) -> Result<Vec<Role>> {
    match funding {
        Funding::Airdrop => {
            let mut everyone = vec![keys.deployer.pubkey()];
            everyone.extend(ROLES.iter().map(|role| keys.address(*role)));
            for who in everyone {
                if client.lamports(&who).await? < STARTING_LAMPORTS / 2 {
                    client.airdrop(&who, STARTING_LAMPORTS).await?;
                }
            }
            Ok(Vec::new())
        }
        Funding::Transfer { reserve, roles } => {
            let deployer = &keys.deployer;
            let mut holdings = Vec::new();
            for (role, _) in roles {
                holdings.push(client.lamports(&keys.address(*role)).await?);
            }
            let available = client
                .lamports(&deployer.pubkey())
                .await?
                .saturating_sub(*reserve);
            let plan = transfers(roles, &holdings, available);
            for (role, holds, need) in &plan.rows {
                let address = keys.address(*role);
                match need {
                    Need::Enough => {
                        tracing::info!(role = role.label(), %address, lamports = holds, "holds its amount")
                    }
                    Need::Sent(lamports) => {
                        tracing::info!(role = role.label(), %address, lamports, "topped up")
                    }
                    Need::Short(lamports) => {
                        tracing::warn!(role = role.label(), %address, lamports, "the deployer can't cover this role past its reserve")
                    }
                }
            }
            let sends: Vec<_> = plan
                .rows
                .iter()
                .filter_map(|(role, _, need)| match need {
                    Need::Sent(lamports) => Some(chain::solana::executor::ix_fund(
                        &deployer.pubkey(),
                        &keys.address(*role),
                        *lamports,
                    )),
                    _ => None,
                })
                .collect();
            if !sends.is_empty() {
                client
                    .send(sends, &[deployer])
                    .await
                    .context("funding the roles from the deployer")?;
            }
            Ok(plan.short())
        }
    }
}

/// What one role is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Need {
    /// It holds its amount already.
    Enough,
    /// This many lamports, to bring it to its amount.
    Sent(u64),
    /// This many lamports, more than the deployer holds past its reserve:
    /// nothing is sent.
    Short(u64),
}

#[derive(Debug, PartialEq, Eq)]
struct Transfers {
    /// Each role, what it holds, and what it is given.
    rows: Vec<(Role, u64, Need)>,
}

impl Transfers {
    fn short(&self) -> Vec<Role> {
        self.rows
            .iter()
            .filter(|(_, _, need)| matches!(need, Need::Short(_)))
            .map(|(role, _, _)| *role)
            .collect()
    }
}

/// Who is topped up, in order, from `available` lamports: each role short of
/// its amount is given exactly what it lacks while the deployer can cover
/// it, and a role it can't cover is given nothing, never part.
fn transfers(roles: &[(Role, u64)], holdings: &[u64], mut available: u64) -> Transfers {
    let rows = roles
        .iter()
        .zip(holdings)
        .map(|((role, target), holds)| {
            let need = target.saturating_sub(*holds);
            let given = if need == 0 {
                Need::Enough
            } else if need <= available {
                available -= need;
                Need::Sent(need)
            } else {
                Need::Short(need)
            };
            (*role, *holds, given)
        })
        .collect();
    Transfers { rows }
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
async fn set_up(
    client: &SolanaClient,
    keys: &RoleKeys,
    settings: &Settings,
) -> Result<(Platform, Pubkey)> {
    let deployer = &keys.deployer;
    let admin = keys.get(Role::Admin);
    let operator = keys.get(Role::Operator);
    let dollar = &settings.mint;
    let stand_ins = dollar.stand_ins();
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
        mint: dollar.pubkey(),
        admin: admin.pubkey(),
        platform: keys.address(Role::Platform),
        operator: operator.pubkey(),
        checker: Some(checker),
    };

    // The dollar: minted by the admin on a development cluster, as anyone
    // may mint the EVM's local one; a public cluster's must already be
    // there.
    if let Dollar::Own(mint) = dollar
        && client.account(&mint.pubkey()).await?.is_none()
    {
        let rent = client
            .rpc
            .rent_exempt_minimum(Platform::rent_for_mint())
            .await?;
        let ixs =
            Platform::ix_create_mint(&deployer.pubkey(), &mint.pubkey(), &admin.pubkey(), rent)?;
        client
            .send(ixs, &[deployer, mint])
            .await
            .context("creating the dollar")?;
    }
    if let Dollar::Public(mint) = dollar
        && client.account(mint).await?.is_none()
    {
        bail!("the dollar's mint {mint} is not on {}", client.key);
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
                timelock: settings.timelock,
                min_timelock: settings.min_timelock,
                blessing_ttl: settings.blessing_ttl,
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
        let ix = platform.ix_initialize_prices(
            &deployer.pubkey(),
            settings.price_notice,
            published_prices(),
        );
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
    if stand_ins
        && client
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
        let ix = platform.ix_initialize_vaults(&deployer.pubkey(), settings.terms);
        client
            .send(vec![ix], &[deployer])
            .await
            .context("setting the vault factory up")?;
    }

    // The venue's markets, and its reserves.
    let markets: &[(&str, bool)] = if stand_ins {
        &[(SPOT, false), (PERP, true)]
    } else {
        &[]
    };
    for &(market, is_perp) in markets {
        if client
            .account(&local_venue::market_address(&name(market)))
            .await?
            .is_none()
        {
            let ix = platform.ix_list_market(name(market), is_perp, STARTING_PRICE);
            client.send(vec![ix], &[operator]).await?;
        }
    }
    // Topped up to them, not added to on every run.
    let reserves = if stand_ins {
        VENUE_RESERVES.saturating_sub(client.token_balance(&local_venue::vault_address()).await?)
    } else {
        0
    };
    if reserves > 0 {
        client
            .send(
                vec![platform.ix_mint_into(
                    &admin.pubkey(),
                    &local_venue::vault_address(),
                    reserves,
                )?],
                &[admin],
            )
            .await
            .context("the venue's reserves")?;
    }
    if stand_ins {
        tracing::info!("the stand-in venue has listed its markets");
    }

    // The images, the project and the stand-in driver, behind the timelock.
    let mut proposals = Vec::new();
    for (digest, kind) in &settings.images {
        if client
            .account(&registry::read::image_address(digest))
            .await?
            .is_none()
        {
            proposals.push(platform.ix_propose_image(*digest, *kind));
        }
    }
    if client
        .account(&registry::read::project_address(&settings.project))
        .await?
        .is_none()
    {
        proposals.push(platform.ix_propose_project(settings.project));
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
    if stand_ins && client.account(&driver_slot).await?.is_none() {
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
    // Orca's driver, deployed final as the registry requires: proposed if
    // the registry approves another program under its name (an earlier
    // build's, listing another dollar's pool) or none.
    let orca_slot =
        registry::read::approval_address(registry::state::ApprovalKind::Driver, &orca_driver::NAME);
    let now = client.now().await?;
    let approved = client
        .load::<registry::state::ApprovalSlot>(&orca_slot)
        .await?
        .is_some_and(|slot| slot.effective(now) == orca_driver::ID || slot.next == orca_driver::ID);
    if !approved {
        let ix = platform
            .ix_propose(
                client,
                registry::state::ApprovalKind::Driver,
                orca_driver::NAME,
                orca_driver::ID,
            )
            .await?;
        client
            .send(vec![ix], &[admin])
            .await
            .context("proposing Orca's driver")?;
    }
    debug_assert_ne!(program_data_address(&local_driver::ID), Pubkey::default());
    tracing::info!("the images and the drivers are proposed; the timelock is running");

    // Money: every role's dollar account, and dollars for those who spend.
    let mut open = Vec::new();
    for role in ROLES {
        open.push(platform.ix_open_dollars(&deployer.pubkey(), &keys.address(role)));
    }
    client.send(open, &[deployer]).await?;
    // Each topped up to its starting dollars, as on the EVM: one that holds
    // them is given nothing. A public cluster's dollar comes from its
    // faucet alone.
    let mut minting = Vec::new();
    let spenders: &[Role] = if stand_ins {
        &[Role::Author, Role::Viewer, Role::Keeper, Role::Admin]
    } else {
        &[]
    };
    for &role in spenders {
        let holds = client
            .token_balance(&platform.dollars(&keys.address(role)))
            .await?;
        if holds < STARTING_DOLLARS {
            minting.push(platform.ix_mint_to(
                &admin.pubkey(),
                &keys.address(role),
                STARTING_DOLLARS - holds,
            )?);
        }
    }
    if !minting.is_empty() {
        client.send(minting, &[admin]).await?;
    }
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
    // Pyth's stand-in posts SOL's price from the tape's moves since SOL's
    // starting price, as Pyth posts its own; the market operator then brings
    // Orca's pool to it and keeps its seed around it.
    let admin = client.role(Role::Admin)?;
    let mut first_spot = None;
    let steering = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    let orca = client
        .account(&chain::solana::orca::POOLS[0].address)
        .await?
        .is_some();
    loop {
        // The clock only moves forward, and runs on at real pace between
        // jumps, so the next mark is always ahead of it.
        let now = client.now().await?;
        while at <= now {
            at += step;
        }
        let tick = tape.step();
        let first = *first_spot.get_or_insert(tick.spot);
        let sol = (crate::solana_orca::LOCAL_START as f64 * tick.spot / first.max(1.0)) as u128;
        // Published at the mark the clock jumps to, so the price is never
        // older than the driver takes, however far the clock jumps.
        if orca && let Err(error) = chain::solana::orca::setup::post_price(client, sol, at).await {
            tracing::warn!(
                error = format!("{error:#}"),
                "Pyth's stand-in would not take the price"
            );
        }
        if let Err(error) = client.rpc.time_travel(at).await {
            tracing::warn!(error = format!("{error:#}"), "the clock would not move");
        }
        if orca && let Ok(turn) = steering.clone().try_lock_owned() {
            let (client, platform) = (client.clone(), platform.clone());
            let (admin, operator) = (admin.insecure_clone(), operator.insecure_clone());
            tokio::spawn(async move {
                let _turn = turn;
                let kept = chain::solana::orca::setup::keep_seed_at(
                    &client, &platform, &admin, &operator, sol, None,
                )
                .await;
                if let Err(error) = kept {
                    tracing::warn!(
                        error = format!("{error:#}"),
                        "Orca's pool would not follow Pyth's stand-in"
                    );
                }
            });
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid phrase that is not the development one (BIP-39's own test
    /// vector), standing for the team's.
    const TEAM: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn funding() -> crate::fund::Funding {
        toml::from_str(
            r#"
[testnet."solana:devnet"]
reserve = 1
default = 0.2
keeper = 2
platform = 0

[hyperliquid]
max_price = 35.0
max_per_run = 3.0
activation_usd = 2.0
"#,
        )
        .unwrap()
    }

    fn devnet() -> ChainKey {
        "solana:devnet".parse().unwrap()
    }

    fn testnet() -> Settings {
        Settings::testnet(TEAM, "shonin-testnet", &funding(), &devnet()).unwrap()
    }

    #[test]
    fn a_public_cluster_is_set_up_with_no_placeholder_image() {
        assert!(testnet().images.is_empty());
        // A development cluster's placeholders are all it has.
        let local = Settings::local().unwrap();
        assert_eq!(local.images.len(), 4);
        assert!(local.images.iter().all(|(digest, _)| {
            ["executor", "builder", "reviewer", "unwinder"]
                .iter()
                .any(|service| image_digest(service) == *digest)
        }));
    }

    #[test]
    fn a_public_cluster_allows_the_operator_s_real_project_not_the_placeholder() {
        let settings = testnet();
        assert_eq!(
            settings.project,
            crate::admin::project_id("shonin-testnet").0,
            "the project is proposed as chains:project proposes it on the EVM"
        );
        assert_ne!(settings.project, project_id());
        for typo in [
            "",
            "Shonin-Testnet",
            "shonin_testnet",
            "shonin-",
            "short",
            "1shonin",
        ] {
            assert!(
                Settings::testnet(TEAM, typo, &funding(), &devnet()).is_err(),
                "{typo:?} was taken as a project"
            );
        }
    }

    #[test]
    fn the_devnet_dollar_is_circle_s_usdc_with_no_stand_ins_and_the_local_one_is_minted_here() {
        let devnet = testnet();
        assert!(matches!(devnet.mint, Dollar::Public(mint) if mint == CIRCLE_DEVNET_USDC));
        assert!(!devnet.mint.stand_ins());
        let local = Settings::local().unwrap();
        assert!(local.mint.stand_ins());
        assert_ne!(local.mint.pubkey(), CIRCLE_DEVNET_USDC);
    }

    #[test]
    #[cfg(feature = "devnet")]
    fn the_devnet_build_s_orca_driver_lists_circle_s_usdc() {
        assert_eq!(chain::solana::orca::DOLLAR_MINT, CIRCLE_DEVNET_USDC);
    }

    #[test]
    fn a_mint_s_authority_is_read_from_its_optional_key() {
        let admin = Pubkey::new_unique();
        let mut data = vec![0u8; 82];
        data[..4].copy_from_slice(&[1, 0, 0, 0]);
        data[4..36].copy_from_slice(admin.as_ref());
        assert_eq!(mint_authority(&data), Some(admin));
        // No authority: nobody mints it.
        data[..4].copy_from_slice(&[0, 0, 0, 0]);
        assert_eq!(mint_authority(&data), None);
        assert_eq!(mint_authority(&[]), None);
    }

    #[test]
    fn a_public_cluster_is_never_set_up_from_the_development_mnemonic() {
        let refused = Settings::testnet(
            &format!("  {}  ", keys::DEVELOPMENT_MNEMONIC),
            "shonin-testnet",
            &funding(),
            &devnet(),
        );
        assert!(refused.is_err());
    }

    #[test]
    fn a_public_cluster_s_roles_are_funded_by_transfer_from_the_deployer_not_the_faucet() {
        let Funding::Transfer { reserve, roles } = testnet().funding else {
            panic!("a public cluster is not funded from its faucet");
        };
        assert_eq!(reserve, LAMPORTS_PER_SOL);
        let of = |role: Role| roles.iter().find(|(r, _)| *r == role).unwrap().1;
        assert_eq!(of(Role::Keeper), 2 * LAMPORTS_PER_SOL);
        assert_eq!(of(Role::Admin), LAMPORTS_PER_SOL / 5);
        assert_eq!(of(Role::Platform), 0);
        assert_eq!(Settings::local().unwrap().funding, Funding::Airdrop);
        // A cluster the funding file doesn't list is refused, not funded
        // with nothing.
        let mainnet: ChainKey = "solana:mainnet-beta".parse().unwrap();
        assert!(Settings::testnet(TEAM, "shonin-testnet", &funding(), &mainnet).is_err());
    }

    #[test]
    fn each_role_is_given_exactly_what_it_lacks_or_nothing() {
        let roles = [
            (Role::Admin, 500),
            (Role::Keeper, 2_000),
            (Role::Author, 1_000),
            (Role::Platform, 0),
            (Role::Viewer, 300),
        ];
        let holdings = [500, 1_200, 0, 7, 100];
        let plan = transfers(&roles, &holdings, 1_000);
        assert_eq!(
            plan.rows,
            vec![
                (Role::Admin, 500, Need::Enough),
                (Role::Keeper, 1_200, Need::Sent(800)),
                // 200 left: the author's 1,000 can't be covered, and it is
                // given none of it.
                (Role::Author, 0, Need::Short(1_000)),
                (Role::Platform, 7, Need::Enough),
                (Role::Viewer, 100, Need::Sent(200)),
            ]
        );
        assert_eq!(plan.short(), vec![Role::Author]);
        // A second run, every amount held, sends nothing.
        let again = transfers(&roles, &[500, 2_000, 1_000, 0, 300], 0);
        assert!(again.rows.iter().all(|(_, _, need)| *need == Need::Enough));
    }

    #[test]
    fn a_public_cluster_has_no_timelock_and_the_testnet_terms() {
        let settings = testnet();
        assert_eq!((settings.timelock, settings.min_timelock), (0, 0));
        assert_eq!(settings.price_notice, 0);
        assert_eq!(settings.blessing_ttl, 72 * HOUR);
        assert_eq!(settings.terms, testnet_terms());
        // The EVM testnets': a $10 tank and deposit, totals fresh for ten
        // minutes, a week to freeze.
        assert_eq!(settings.terms.minimum_tank, 10_000_000);
        assert_eq!(settings.terms.minimum_deposit, 10_000_000);
        assert_eq!(settings.terms.timers.usage_freshness, 10 * MINUTE);
        assert_eq!(settings.terms.timers.freeze_after, 7 * DAY);
    }

    #[test]
    #[cfg(not(any(feature = "devnet", feature = "mainnet")))]
    fn the_local_dollar_is_the_one_orca_s_driver_lists_for_a_local_cluster() {
        assert_eq!(
            Settings::local().unwrap().mint.pubkey(),
            chain::solana::orca::DOLLAR_MINT
        );
    }

    #[test]
    fn orca_s_driver_is_deployed_final_as_every_driver_is() {
        assert!(deployed_final("shonin_orca_driver"));
        assert!(deployed_final("shonin_local_driver"));
        assert!(!deployed_final("shonin_station"));
    }

    #[test]
    fn every_cluster_s_terms_are_within_the_vault_program_s_bounds() {
        vault::factory::checked(&testnet_terms()).unwrap();
        vault::factory::checked(&terms()).unwrap();
    }
}
