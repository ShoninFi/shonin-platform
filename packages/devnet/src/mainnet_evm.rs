//! The EVM mainnet deploy: exactly what ships on each chain, and nothing
//! else.
//!
//! The plan ([`plan_chain`]) creates only the contracts in [`SHIPPING`]:
//! RISC Zero's Groth16 verifier and the Confidential Space checker (the
//! registry's first and only checker, trusting Google's Confidential Space
//! root alone), the platform's contracts with a two-day timelock that can
//! never be lowered and a thirty-day notice on the price list, and the
//! chain's one driver, built from the listings in `config/mainnet.toml`.
//! It never reaches the development plan, so no stand-in dollar, venue,
//! driver or bridge, no development checker, no mint and no placeholder
//! image or project can be part of it.
//!
//! The deployer (a hot key the owner funds with just enough gas) sends the
//! creations alone. Every contract names the owner's hardware key (`ADMIN`)
//! as its admin from its creation, and the platform's fees go there too.
//! What only the admin may do (initialising the station factory, proposing
//! the driver, the project when it is known) is written as prepared
//! transactions to a file ([`AdminSteps`]), for the hardware key to sign;
//! on a fork rehearsal they are sent as the admin, impersonated. The
//! deployer holds no power once the deploy ends.
//!
//! Every address follows from the build, the admin and the generation,
//! through the deterministic deployer, before anything is sent: a dry run
//! prints them all, with every constructor's arguments and every admin step.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol_types::{SolCall, SolConstructor, SolValue};
use anyhow::{Context, Result, anyhow, bail};
use chain::{ChainConfig, ChainKey, Client, Deployments, NetworkName, Roles, Signing};
use serde::{Deserialize, Serialize};
use shonin_attestation_deploy::{BN254_CONTROL_ID, CONTROL_ROOT, GUEST_IMAGE_ID};

use crate::deploy::{
    self, BLESSING_TTL, CREATE2_DEPLOYER, Creation, Landed, MAINNET_MIN_TIMELOCK, Parties,
    PlatformAddresses, StationFactorySet, Terms,
};
use crate::mainnet_guard::{self, Mode};

/// Every contract the mainnet deploy may create. Anything else is refused
/// by construction: the plan names these alone.
pub const SHIPPING: [&str; 20] = [
    "RiscZeroGroth16Verifier",
    "ConfidentialSpaceVerifier",
    "AttestationRegistry",
    "Publishing",
    "Prices",
    "StrategyRegistry",
    "ReviewRegistry",
    "AuthorProfiles",
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
    "RobinhoodDriver",
    "HyperliquidDriver",
];

/// The development contracts, which never reach a mainnet: a dollar anyone
/// may mint, the stand-in venue, driver and bridge, and a checker that
/// trusts one development key.
pub const NEVER_ON_MAINNET: [&str; 5] = [
    "LocalUsd",
    "LocalVenue",
    "LocalDriver",
    "LocalBridge",
    "DevAttestationVerifier",
];

/// The notice a change to the price list is published with on mainnet.
pub const MAINNET_PRICE_NOTICE: u64 = 30 * 24 * 60 * 60;

/// What a deployment file records of the mainnet deploy beyond the
/// addresses every reader knows.
pub const MAINNET_KEY: &str = "mainnet";

// --- The outside contracts, from config/mainnet.toml ------------------------

/// `config/mainnet.toml`: the outside contracts each mainnet's deploy
/// builds on.
#[derive(Clone, Debug, Deserialize)]
pub struct Venues {
    pub evm: EvmVenues,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EvmVenues {
    pub create2_deployer: Address,
    pub safe_singleton: Address,
    pub safe_proxy_factory: Address,
    pub roles_mastercopy: Address,
    pub module_proxy_factory: Address,
    /// By chain id.
    pub chains: BTreeMap<String, ChainVenues>,
}

/// One chain's dollar and its one driver.
#[derive(Clone, Debug, Deserialize)]
pub struct ChainVenues {
    pub label: String,
    pub dollar: Address,
    pub dollar_symbol: String,
    /// The Robinhood driver's contract over Uniswap v3 pools.
    pub uniswap: Option<UniswapDriver>,
    pub hyperliquid: Option<HyperliquidDriver>,
}

/// The most listings the Robinhood driver's constructor takes
/// (`MAX_LISTINGS` in its contract).
pub const MAX_LISTINGS: usize = 2;

#[derive(Clone, Debug, Deserialize)]
pub struct UniswapDriver {
    /// The name `config/drivers.toml` lists it under.
    pub driver: String,
    pub router: Address,
    /// Seconds before a feed's price stops being trusted.
    pub max_feed_age: u64,
    /// Seconds of each pool's time-weighted price.
    pub twap_window: u32,
    pub listings: Vec<UniswapListing>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UniswapListing {
    pub symbol: String,
    /// The listed token's address, written `asset` in the file.
    #[serde(rename = "asset")]
    pub token: Address,
    pub feed: Address,
    pub pool: Address,
    /// The most of the token, in its base units, that one sale of a stopped
    /// station's full unwind sells (the driver refuses zero).
    pub max_sale: U256,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HyperliquidDriver {
    pub driver: String,
    pub deposit_wallet: Address,
    pub perps: Vec<u32>,
    pub spot_pairs: Vec<u32>,
}

impl Venues {
    pub fn read(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let venues: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        if venues.evm.create2_deployer != CREATE2_DEPLOYER {
            bail!(
                "{} names {} as the deterministic deployer, and the deploy computes every address \
                 through {CREATE2_DEPLOYER}",
                path.display(),
                venues.evm.create2_deployer
            );
        }
        venues
            .check_listings()
            .with_context(|| format!("checking {}", path.display()))?;
        Ok(venues)
    }

    /// Each Robinhood driver's listings as its constructor takes them: one
    /// to [`MAX_LISTINGS`], each with a maximum sale. The constructor
    /// refuses anything else, so it is refused here before anything is
    /// planned or sent.
    pub fn check_listings(&self) -> Result<()> {
        for (chain_id, chain) in &self.evm.chains {
            let Some(uniswap) = &chain.uniswap else {
                continue;
            };
            if uniswap.listings.is_empty() || uniswap.listings.len() > MAX_LISTINGS {
                bail!(
                    "chain {chain_id} ({}) lists {} tokens, and the driver takes 1 to {MAX_LISTINGS}",
                    chain.label,
                    uniswap.listings.len()
                );
            }
            for listing in &uniswap.listings {
                if listing.max_sale.is_zero() {
                    bail!(
                        "chain {chain_id} ({}) lists {} with no max_sale, and the driver refuses zero",
                        chain.label,
                        listing.symbol
                    );
                }
            }
        }
        Ok(())
    }

    /// The chain's dollar and driver; an error for a chain the file doesn't
    /// list, or one with no driver or two.
    pub fn chain(&self, chain_id: u64) -> Result<&ChainVenues> {
        let chain = self
            .evm
            .chains
            .get(&chain_id.to_string())
            .ok_or_else(|| anyhow!("config/mainnet.toml lists nothing for chain {chain_id}"))?;
        match (&chain.uniswap, &chain.hyperliquid) {
            (Some(_), None) | (None, Some(_)) => Ok(chain),
            _ => bail!(
                "config/mainnet.toml must list exactly one driver for chain {chain_id} ({})",
                chain.label
            ),
        }
    }
}

// --- The plan ---------------------------------------------------------------

alloy::sol! {
    /// The Robinhood driver's constructor
    /// (`contracts/evm/src/drivers/robinhood/RobinhoodDriver.sol`).
    contract MainnetRobinhoodDriver {
        struct DriverVenue {
            address dollar;
            address router;
            address safeSingleton;
            address safeFactory;
            address rolesMastercopy;
            address moduleFactory;
            uint256 maxFeedAge;
            uint32 twapWindow;
        }
        struct DriverListing {
            address token;
            address feed;
            address pool;
            uint256 maxSale;
        }
        constructor(DriverVenue venue, DriverListing[] listings);
    }
}

/// The chain's one driver, as planned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedDriver {
    /// The name the registry approves it under.
    pub name: String,
    pub contract: &'static str,
    pub address: Address,
}

/// One chain's mainnet deployment, worked out from the build alone.
#[derive(Clone, Debug)]
pub struct ChainPlan {
    pub chain_id: u64,
    pub label: String,
    pub admin: Address,
    pub generation: u64,
    pub dollar: Address,
    pub creations: Vec<Creation>,
    pub risc_zero: Address,
    pub checker: Address,
    pub platform: PlatformAddresses,
    pub driver: PlannedDriver,
    /// What each creation's arguments mean, by contract, for the dry run.
    pub described: BTreeMap<&'static str, String>,
}

/// What the mainnet checker trusts: Google's Confidential Space root CA
/// alone. Every machine asks for PKI tokens, and the root outlives any
/// pinned signing key by years.
pub fn mainnet_trusted() -> Vec<B256> {
    vec![B256::from(
        shonin_attestation_core::confidential_space_root_hash(),
    )]
}

/// The terms the platform contracts are created with on mainnet: a two-day
/// timelock, two days its floor for good, and thirty days' notice on the
/// price list.
pub fn mainnet_terms() -> Terms {
    Terms {
        timelock: MAINNET_MIN_TIMELOCK,
        min_timelock: MAINNET_MIN_TIMELOCK,
        price_notice: MAINNET_PRICE_NOTICE,
    }
}

/// Every contract chain `chain_id`'s mainnet deployment creates, and where,
/// for admin `admin` under `generation`: nothing is asked of a chain.
pub fn plan_chain(
    artifacts: &Path,
    venues: &Venues,
    chain_id: u64,
    admin: Address,
    generation: u64,
    names: &[(String, u64)],
) -> Result<ChainPlan> {
    let chain = venues.chain(chain_id)?;
    let own = deploy::deployment_salt(generation, admin);
    let mut creations = Vec::new();
    let mut described = BTreeMap::new();
    let add = |name: &'static str, arguments: Vec<u8>, creations: &mut Vec<Creation>| {
        debug_assert!(SHIPPING.contains(&name));
        let creation = Creation::new(artifacts, name, own, arguments, &[])?;
        let address = creation.address;
        creations.push(creation);
        anyhow::Ok(address)
    };

    let risc_zero = add(
        "RiscZeroGroth16Verifier",
        (CONTROL_ROOT, BN254_CONTROL_ID).abi_encode_params(),
        &mut creations,
    )?;
    described.insert(
        "RiscZeroGroth16Verifier",
        format!("control root {CONTROL_ROOT}, BN254 control id {BN254_CONTROL_ID}"),
    );
    let trusted = mainnet_trusted();
    let checker = add(
        "ConfidentialSpaceVerifier",
        (risc_zero, GUEST_IMAGE_ID, trusted.clone()).abi_encode_params(),
        &mut creations,
    )?;
    described.insert(
        "ConfidentialSpaceVerifier",
        format!(
            "RISC Zero's verifier {risc_zero}, guest image id {GUEST_IMAGE_ID}, trusts {} \
             (Google's Confidential Space root CA alone)",
            trusted[0]
        ),
    );
    let terms = mainnet_terms();
    let platform = deploy::platform_contracts(
        artifacts,
        NetworkName::Mainnet,
        &terms,
        &Parties {
            admin,
            // The platform's fees go to the admin until a multisig exists.
            platform: admin,
            usd: chain.dollar,
            verifier: checker,
        },
        names,
        generation,
        &mut creations,
    )?;
    described.insert(
        "AttestationRegistry",
        format!(
            "admin {admin}, first checker {checker}, timelock {} s, floor {} s, blessing {} s",
            terms.timelock, terms.min_timelock, BLESSING_TTL
        ),
    );
    described.insert(
        "Publishing",
        format!(
            "dollar {} ({}), fees to {admin}",
            chain.dollar, chain.dollar_symbol
        ),
    );
    described.insert(
        "Prices",
        format!("admin {admin}, notice {} s", terms.price_notice),
    );
    described.insert(
        "StationFactory",
        format!("initialised by {admin} alone (an admin step)"),
    );
    described.insert(
        "VaultFactory",
        format!(
            "dollar {}, admin {admin}, platform {admin}, chains {}",
            chain.dollar,
            names
                .iter()
                .map(|(name, id)| format!("{name}={id}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    );

    let driver = match (&chain.uniswap, &chain.hyperliquid) {
        (Some(uniswap), None) => {
            let arguments = MainnetRobinhoodDriver::constructorCall {
                venue: MainnetRobinhoodDriver::DriverVenue {
                    dollar: chain.dollar,
                    router: uniswap.router,
                    safeSingleton: venues.evm.safe_singleton,
                    safeFactory: venues.evm.safe_proxy_factory,
                    rolesMastercopy: venues.evm.roles_mastercopy,
                    moduleFactory: venues.evm.module_proxy_factory,
                    maxFeedAge: U256::from(uniswap.max_feed_age),
                    twapWindow: uniswap.twap_window,
                },
                listings: uniswap
                    .listings
                    .iter()
                    .map(|listing| MainnetRobinhoodDriver::DriverListing {
                        token: listing.token,
                        feed: listing.feed,
                        pool: listing.pool,
                        maxSale: listing.max_sale,
                    })
                    .collect(),
            }
            .abi_encode();
            let address = add("RobinhoodDriver", arguments, &mut creations)?;
            described.insert(
                "RobinhoodDriver",
                format!(
                    "as {}: dollar {}, router {}, max feed age {} s, TWAP {} s, listings {}",
                    uniswap.driver,
                    chain.dollar,
                    uniswap.router,
                    uniswap.max_feed_age,
                    uniswap.twap_window,
                    uniswap
                        .listings
                        .iter()
                        .map(|listing| format!(
                            "{} (token {}, feed {}, pool {}, max sale {})",
                            listing.symbol,
                            listing.token,
                            listing.feed,
                            listing.pool,
                            listing.max_sale
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            );
            PlannedDriver {
                name: uniswap.driver.clone(),
                contract: "RobinhoodDriver",
                address,
            }
        }
        (None, Some(hyperliquid)) => {
            let address = add(
                "HyperliquidDriver",
                (
                    chain.dollar,
                    hyperliquid.deposit_wallet,
                    hyperliquid.perps.clone(),
                    hyperliquid.spot_pairs.clone(),
                )
                    .abi_encode_params(),
                &mut creations,
            )?;
            described.insert(
                "HyperliquidDriver",
                format!(
                    "as {}: USDC {}, deposit wallet {}, perps {:?}, spot pairs {:?}",
                    hyperliquid.driver,
                    chain.dollar,
                    hyperliquid.deposit_wallet,
                    hyperliquid.perps,
                    hyperliquid.spot_pairs
                ),
            );
            PlannedDriver {
                name: hyperliquid.driver.clone(),
                contract: "HyperliquidDriver",
                address,
            }
        }
        _ => unreachable!("Venues::chain lets exactly one driver through"),
    };

    for creation in &creations {
        if !SHIPPING.contains(&creation.name) || NEVER_ON_MAINNET.contains(&creation.name) {
            bail!("{} is not among the contracts that ship", creation.name);
        }
    }
    Ok(ChainPlan {
        chain_id,
        label: chain.label.clone(),
        admin,
        generation,
        dollar: chain.dollar,
        creations,
        risc_zero,
        checker,
        platform,
        driver,
        described,
    })
}

// --- The admin's steps ------------------------------------------------------

alloy::sol! {
    /// What the mainnet deploy reads of, and asks the admin to send to, the
    /// platform's contracts.
    interface IMainnetRegistry {
        function admin() external view returns (address);
        function timelock() external view returns (uint64);
        function minTimelock() external view returns (uint64);
        function blessingTtl() external view returns (uint64);
        function verifiers() external view returns (address[] memory);
        function isVerifierAllowed(address verifier) external view returns (bool);
        function tradingDriver(bytes32 name) external view returns (address);
        function bridgeOf(bytes32 name) external view returns (address);
        function isProjectAllowed(bytes32 projectId) external view returns (bool);
        function proposeDriver(bytes32 name, address implementation) external;
        function proposeProject(bytes32 projectId) external;
        event DriverProposed(bytes32 indexed name, address indexed implementation, uint64 readyAt);
    }
    interface IMainnetChecker {
        function riscZero() external view returns (address);
        function imageId() external view returns (bytes32);
        function trustedKeys() external view returns (bytes32[] memory);
    }
    interface IMainnetPrices {
        function admin() external view returns (address);
        function notice() external view returns (uint64);
    }
    interface IMainnetPublishing {
        function platform() external view returns (address);
        function asset() external view returns (address);
    }
    interface IMainnetVaultFactory {
        function admin() external view returns (address);
        function platform() external view returns (address);
    }
    interface IMainnetStationFactory {
        function initializer() external view returns (address);
        function initialize(address attestation_, address asset_, address vaultFactory_, address implementation_) external;
    }
    interface IMainnetRobinhoodDriver {
        struct Stock {
            address token;
            address feed;
            address pool;
            uint24 fee;
            uint256 scale;
        }
        function dollar() external view returns (address);
        function router() external view returns (address);
        function maxFeedAge() external view returns (uint256);
        function twapWindow() external view returns (uint32);
        function stocks() external view returns (Stock[] memory);
        function maxSale(address token) external view returns (uint256);
    }
    interface IMainnetHyperliquidDriver {
        struct Perp {
            uint32 index;
            uint8 szDecimals;
        }
        struct Spot {
            uint32 pair;
            uint64 token;
            uint8 szDecimals;
            uint8 weiDecimals;
        }
        function usdc() external view returns (address);
        function depositWallet() external view returns (address);
        function perps() external view returns (Perp[] memory);
        function spots() external view returns (Spot[] memory);
    }
}

/// What an admin step does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepKind {
    InitialiseStationFactory,
    ProposeDriver,
    ProposeProject,
}

/// One transaction only the admin may send, prepared for the hardware key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminStep {
    pub chain_id: u64,
    pub to: Address,
    pub data: Bytes,
    pub value: U256,
    pub purpose: String,
    pub kind: StepKind,
}

/// A chain's admin steps, as written for the hardware key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminSteps {
    pub chain_id: u64,
    pub admin: Address,
    pub registry: Address,
    /// The block the deployment began at: proposals are looked for from
    /// there.
    pub from_block: u64,
    pub steps: Vec<AdminStep>,
}

impl AdminSteps {
    pub fn file_in(dir: &Path, chain_id: u64) -> PathBuf {
        dir.join(format!("admin-steps-evm-{chain_id}.json"))
    }
}

/// A Google Cloud project as the registry holds it.
pub fn project_hash(project: &str) -> B256 {
    keccak256(project.as_bytes())
}

/// The admin's steps for a planned chain: initialise the station factory
/// for this deployment, propose the chain's driver, and the operator's
/// project when it is given. Never an image: those are real digests,
/// proposed once they exist.
pub fn admin_steps(plan: &ChainPlan, project: Option<&str>) -> Vec<AdminStep> {
    let registry = plan.platform.attestation;
    let mut steps = vec![
        AdminStep {
            chain_id: plan.chain_id,
            to: plan.platform.station_factory,
            data: IMainnetStationFactory::initializeCall {
                attestation_: registry,
                asset_: plan.dollar,
                vaultFactory_: plan.platform.factory,
                implementation_: plan.platform.station_implementation,
            }
            .abi_encode()
            .into(),
            value: U256::ZERO,
            purpose: format!(
                "initialise the station factory {} for this deployment (registry {registry}, \
                 dollar {}, vault factory {}, station {})",
                plan.platform.station_factory,
                plan.dollar,
                plan.platform.factory,
                plan.platform.station_implementation
            ),
            kind: StepKind::InitialiseStationFactory,
        },
        AdminStep {
            chain_id: plan.chain_id,
            to: registry,
            data: IMainnetRegistry::proposeDriverCall {
                name: deploy::market_id(&plan.driver.name),
                implementation: plan.driver.address,
            }
            .abi_encode()
            .into(),
            value: U256::ZERO,
            purpose: format!(
                "propose the driver {} ({} at {}); it trades once the timelock has passed",
                plan.driver.name, plan.driver.contract, plan.driver.address
            ),
            kind: StepKind::ProposeDriver,
        },
    ];
    if let Some(project) = project.map(str::trim).filter(|project| !project.is_empty()) {
        steps.push(AdminStep {
            chain_id: plan.chain_id,
            to: registry,
            data: IMainnetRegistry::proposeProjectCall {
                projectId: project_hash(project),
            }
            .abi_encode()
            .into(),
            value: U256::ZERO,
            purpose: format!("propose the operator's project {project}"),
            kind: StepKind::ProposeProject,
        });
    }
    steps
}

/// The command that sends one admin step from the hardware wallet.
pub fn ledger_command(step: &AdminStep, admin: Address, public_rpc: &str) -> String {
    format!(
        "cast send --ledger --from {admin} --chain {} --rpc-url {public_rpc} {} {}",
        step.chain_id, step.to, step.data
    )
}

// --- Whether the generation is free -----------------------------------------

/// What is at a generation's station factory address on one chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StationFactoryState {
    Absent,
    /// Created, not yet initialised: only this build's creation code with
    /// this admin puts it there, and only the admin may initialise it.
    Uninitialised,
    Initialised(StationFactorySet),
}

/// Refuse a generation taken on a chain: its station factory initialised
/// for anything but this deployment. One not there, or there and not yet
/// initialised (only this deploy's creation code, with this admin, puts it
/// there), or initialised for exactly this deployment (an earlier try of
/// this deploy) is free.
pub fn judge_generation(
    label: &str,
    generation: u64,
    state: StationFactoryState,
    wanted: StationFactorySet,
) -> Result<()> {
    match state {
        StationFactoryState::Absent | StationFactoryState::Uninitialised => Ok(()),
        StationFactoryState::Initialised(set) if set == wanted => Ok(()),
        StationFactoryState::Initialised(set) => bail!(
            "generation {generation} is taken on {label}: its station factory is initialised for \
             another deployment (vault factory {}, dollar {}, registry {}). Nothing was sent. \
             Deploy every chain under a generation free on all of them.",
            set.vault_factory,
            set.asset,
            set.attestation
        ),
    }
}

async fn station_factory_state(client: &Client, at: Address) -> Result<StationFactoryState> {
    if !client.has_code(at).await? {
        return Ok(StationFactoryState::Absent);
    }
    Ok(match deploy::station_factory_set(client, at).await? {
        Some(set) => StationFactoryState::Initialised(set),
        None => StationFactoryState::Uninitialised,
    })
}

fn wanted_set(plan: &ChainPlan) -> StationFactorySet {
    StationFactorySet {
        attestation: plan.platform.attestation,
        asset: plan.dollar,
        vault_factory: plan.platform.factory,
        implementation: plan.platform.station_implementation,
    }
}

// --- The deploy -------------------------------------------------------------

/// Everything a mainnet deploy run needs, from the command line.
pub struct DeployInputs<'a> {
    pub chains: Vec<(ChainKey, ChainConfig)>,
    pub names: Vec<(String, u64)>,
    pub artifacts: &'a Path,
    pub venues: &'a Venues,
    pub deployments_dir: &'a Path,
    pub admin_steps_dir: &'a Path,
    pub project: Option<String>,
    pub flags: mainnet_guard::Flags,
    /// On a fork alone: replace a recorded deployment file whose factory has
    /// no code on the fork.
    pub overwrite_fork_record: bool,
}

/// The deployment file's mainnet record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MainnetRecord {
    pub risc_zero: Address,
    pub vault_implementation: Address,
    pub drivers: BTreeMap<String, Address>,
    pub deployer: Address,
    pub build: B256,
}

/// Run the mainnet deploy: a dry run, a fork rehearsal or a real send, as
/// the flags decide.
pub async fn run(inputs: DeployInputs<'_>) -> Result<()> {
    let ids: Vec<u64> = inputs
        .chains
        .iter()
        .filter_map(|(key, _)| key.evm_chain_id())
        .collect();
    let decided = mainnet_guard::decide(&inputs.flags, &ids)?;
    if inputs.overwrite_fork_record && decided.mode != Mode::Fork {
        bail!(
            "OVERWRITE_FORK_RECORD is a fork rehearsal's alone: a mainnet deployment file is never \
             overwritten by another deployment. Nothing was sent."
        );
    }
    let deployments_dir = mainnet_guard::deployments_dir(decided.mode, inputs.deployments_dir)?;
    mainnet_guard::check_admin_steps_dir(decided.mode, inputs.admin_steps_dir)?;
    let mut plans = Vec::new();
    for (key, config) in &inputs.chains {
        let id = key
            .evm_chain_id()
            .ok_or_else(|| anyhow!("{key} is not an EVM chain"))?;
        let plan = plan_chain(
            inputs.artifacts,
            inputs.venues,
            id,
            decided.admin,
            decided.generation,
            &inputs.names,
        )?;
        plans.push((key.clone(), config.clone(), plan));
    }
    let build = deploy::build_of_these(inputs.artifacts, &SHIPPING)?;
    let project = inputs.project.as_deref();

    if decided.mode == Mode::DryRun {
        for (key, config, plan) in &plans {
            print_plan(inputs.artifacts, key, config, plan, project);
            // Whether the generation is free, where the chain answers: a
            // read, never a send, and of the mainnet alone (never a fork).
            let read = async {
                mainnet_guard::check_chain(Mode::DryRun, config, plan.chain_id).await?;
                chain::connect_with(key, config, NetworkName::Mainnet, Signing::ReadOnly).await
            };
            match read.await {
                Ok(client) => {
                    let state =
                        station_factory_state(&client, plan.platform.station_factory).await?;
                    match judge_generation(&plan.label, plan.generation, state, wanted_set(plan)) {
                        Ok(()) => println!(
                            "  generation {}: free on {} ({state:?} at the station factory's address)",
                            plan.generation, plan.label
                        ),
                        Err(error) => println!("  {error:#}"),
                    }
                }
                Err(error) => println!("  (the chain was not read: {error:#})"),
            }
        }
        println!(
            "\nA dry run: nothing was sent. A real send takes DEPLOYER_KEYFILE=<path> and \
             CONFIRM_MAINNET={}; a rehearsal on local forks, ON_FORK=1.",
            mainnet_guard::confirmation_for(&ids)
        );
        return Ok(());
    }

    // The deployer's key: the owner's file on a real send; on a fork, a
    // throwaway key the fork funds, or one of anvil's public keys from the
    // file named (never a real one: its transactions would be valid on
    // mainnet).
    let deployer = match (&inputs.flags.deployer_keyfile, decided.mode) {
        (Some(path), Mode::Fork) => mainnet_guard::read_fork_key_file(path)?,
        (Some(path), _) => mainnet_guard::read_key_file(path)?,
        (None, Mode::Fork) => PrivateKeySigner::random(),
        (None, _) => bail!("sending is refused without DEPLOYER_KEYFILE"),
    };
    if deployer.address() == decided.admin {
        bail!(
            "the deployer's key is the admin's address: the admin is the hardware key, and the \
             deployer a hot key that holds no power. Nothing was sent."
        );
    }
    println!(
        "deployer {} (holds no power once the deploy ends), admin {}, generation {}",
        deployer.address(),
        decided.admin,
        decided.generation
    );

    // Every chain is looked at before anything is sent to any.
    let mut connected = Vec::new();
    for (key, config, plan) in plans {
        mainnet_guard::check_chain(decided.mode, &config, plan.chain_id).await?;
        let client = chain::connect_with(
            &key,
            &config,
            NetworkName::Mainnet,
            Signing::Keys(vec![deployer.clone()]),
        )
        .await?;
        mainnet_guard::check_chain_id(&config.label, plan.chain_id, client.chain_id)?;
        if decided.mode == Mode::Fork && hyperliquid::Api::for_chain(plan.chain_id).is_some() {
            hypercore_reads_on_fork(&config, inputs.venues, plan.chain_id).await?;
        }
        let state = station_factory_state(&client, plan.platform.station_factory).await?;
        judge_generation(&plan.label, plan.generation, state, wanted_set(&plan))?;
        refuse_another_recorded(
            &client,
            &Deployments::file_in(&deployments_dir, &key),
            &plan,
            inputs.overwrite_fork_record,
        )
        .await?;
        connected.push((key, config, plan, client));
    }

    for (_, config, plan, client) in &connected {
        if decided.mode == Mode::Fork {
            fund_on_fork(client, deployer.address(), &config.label).await?;
        }
        if hyperliquid::Api::for_chain(plan.chain_id).is_some() {
            big_blocks(decided.mode, &deployer, &config.label).await?;
        }
    }

    let started = std::time::Instant::now();
    let each = connected.iter().map(|(key, _, plan, client)| {
        let deployer = &deployer;
        let deployments_dir = &deployments_dir;
        async move {
            let began = std::time::Instant::now();
            let file = Deployments::file_in(deployments_dir, key);
            let outcome = deploy_chain(
                client,
                key,
                plan,
                deployer,
                inputs.artifacts,
                &file,
                build,
                inputs.overwrite_fork_record,
            )
            .await;
            (key, plan, file, began.elapsed(), outcome)
        }
    });
    let mut stopped = Vec::new();
    for (key, plan, file, took, outcome) in futures::future::join_all(each).await {
        match outcome {
            Ok(from_block) => {
                let steps = AdminSteps {
                    chain_id: plan.chain_id,
                    admin: plan.admin,
                    registry: plan.platform.attestation,
                    from_block,
                    steps: admin_steps(plan, project),
                };
                let steps_file = AdminSteps::file_in(inputs.admin_steps_dir, plan.chain_id);
                write_json(&steps_file, &steps)?;
                println!(
                    "{key}: created in {:.0} s; addresses in {}; the admin's {} steps in {}",
                    took.as_secs_f64(),
                    file.display(),
                    steps.steps.len(),
                    steps_file.display()
                );
            }
            Err(error) => {
                println!(
                    "{key}: stopped after {:.0} s: {error:#}",
                    took.as_secs_f64()
                );
                stopped.push(format!("{key}: {error:#}"));
            }
        }
    }
    println!(
        "every chain done in {:.0} s",
        started.elapsed().as_secs_f64()
    );
    if !stopped.is_empty() {
        bail!(
            "{} of {} chains stopped; run the deploy again to carry on: {}",
            stopped.len(),
            connected.len(),
            stopped.join("; ")
        );
    }
    println!(
        "Next: the admin's steps ({}), then, once the timelock has passed, the read-back.",
        match decided.mode {
            Mode::Fork => "task chains:admin-steps NETWORK=mainnet ON_FORK=1 ADMIN=<address>",
            _ =>
                "task chains:admin-steps NETWORK=mainnet ADMIN=<address> prints the hardware key's commands",
        }
    );
    Ok(())
}

/// Create one chain's contracts not yet there, from the deployer, and write
/// its deployment file; the block it began at. A deployment already on the
/// chain is kept. A recorded file naming another deployment than this
/// plan's is refused whether or not its contracts are on the chain (on a
/// fork, `overwrite_fork_record` replaces one whose factory has no code).
#[allow(clippy::too_many_arguments)]
async fn deploy_chain(
    client: &Client,
    key: &ChainKey,
    plan: &ChainPlan,
    deployer: &PrivateKeySigner,
    artifacts: &Path,
    file: &Path,
    build: B256,
    overwrite_fork_record: bool,
) -> Result<u64> {
    let other = refuse_another_recorded(client, file, plan, overwrite_fork_record).await?;
    if !other && let Some(existing) = deploy::still_there(client, file).await? {
        tracing::info!(file = %file.display(), "the contracts are already on this chain");
        return Ok(existing.deployed_block);
    }
    let mut landed = Landed::load(file, plan.generation);
    let from_block = landed.first_block(client.block_number().await?);
    landed.build = Some(build);
    landed.save()?;
    deploy::create(
        client,
        deployer.address(),
        Some(deployer),
        artifacts,
        &plan.creations,
        &mut landed,
    )
    .await?;
    let deployments = deployments_of(key, plan, from_block);
    let record = MainnetRecord {
        risc_zero: plan.risc_zero,
        vault_implementation: plan.platform.vault_implementation,
        drivers: BTreeMap::from([(plan.driver.name.clone(), plan.driver.address)]),
        deployer: deployer.address(),
        build,
    };
    write_deployment(file, &deployments, plan, &record)?;
    Landed::finished(file);
    Ok(from_block)
}

/// Refuse when `file` records another deployment than `plan`'s, whether or
/// not its contracts are on the chain: a recorded deployment is never
/// overwritten. On a fork alone, `overwrite_fork_record` lets one whose
/// factory has no code there be replaced. Whether the file records another.
async fn refuse_another_recorded(
    client: &Client,
    file: &Path,
    plan: &ChainPlan,
    overwrite_fork_record: bool,
) -> Result<bool> {
    let Some(recorded) = recorded_other_deployment(file, plan)? else {
        return Ok(false);
    };
    let there = client
        .has_code(recorded.factory)
        .await
        .context("reading whether the recorded deployment's factory is on the chain")?;
    if there || !overwrite_fork_record {
        bail!(
            "{} records another deployment (vault factory {}, {}) than this plan's ({}): a \
             recorded deployment is never overwritten. Check ADMIN and GENERATION, or move \
             the file aside if it is known to be wrong. Nothing was sent.",
            file.display(),
            recorded.factory,
            if there {
                "on the chain"
            } else {
                "with no code on the chain"
            },
            plan.platform.factory
        );
    }
    println!(
        "{}: replacing {}, whose deployment is not on the fork (OVERWRITE_FORK_RECORD)",
        plan.label,
        file.display()
    );
    Ok(true)
}

/// The deployment `file` records, when it records one other than `plan`'s
/// (another vault factory or registry, or another generation).
fn recorded_other_deployment(file: &Path, plan: &ChainPlan) -> Result<Option<Deployments>> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", file.display())),
    };
    let recorded = Deployments::from_json(&text)
        .with_context(|| format!("{} is not a deployment file", file.display()))?;
    let same = recorded.factory == plan.platform.factory
        && recorded.attestation == plan.platform.attestation
        && deploy::recorded_generation(&text) == Some(plan.generation);
    Ok((!same).then_some(recorded))
}

/// The deployment file's addresses: no stand-in venue, driver or bridge
/// (the drivers are in the mainnet record), and of the roles only the
/// admin and the platform's fee address, which is the admin's.
pub fn deployments_of(key: &ChainKey, plan: &ChainPlan, from_block: u64) -> Deployments {
    let p = &plan.platform;
    Deployments {
        chain_key: key.to_string(),
        chain_id: plan.chain_id,
        rpc_url: String::new(),
        deployed_block: from_block,
        usd: plan.dollar,
        venue: Address::ZERO,
        attestation_verifier: plan.checker,
        attestation: p.attestation,
        publishing: p.publishing,
        prices: p.prices,
        strategies: p.strategies,
        reviews: p.reviews,
        profiles: p.profiles,
        factory: p.factory,
        station_factory: p.station_factory,
        driver: Address::ZERO,
        bridge: Address::ZERO,
        closer: p.closer,
        roles: Roles {
            admin: plan.admin,
            platform: plan.admin,
            attestor: Address::ZERO,
            builder: Address::ZERO,
            reviewer: Address::ZERO,
            executor: Address::ZERO,
            keeper: Address::ZERO,
            operator: Address::ZERO,
            author: Address::ZERO,
            viewer: Address::ZERO,
        },
    }
}

fn write_deployment(
    file: &Path,
    deployments: &Deployments,
    plan: &ChainPlan,
    record: &MainnetRecord,
) -> Result<()> {
    let mut value: serde_json::Value =
        serde_json::from_str(&deploy::deployment_json(deployments, plan.generation)?)?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            deploy::STATION_IMPLEMENTATION_KEY.to_string(),
            serde_json::json!(plan.platform.station_implementation),
        );
        object.insert(MAINNET_KEY.to_string(), serde_json::to_value(record)?);
    }
    write_json(file, &value)
}

fn write_json(file: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    std::fs::write(file, serde_json::to_string_pretty(value)?)
        .with_context(|| format!("writing {}", file.display()))
}

fn print_plan(
    artifacts: &Path,
    key: &ChainKey,
    config: &ChainConfig,
    plan: &ChainPlan,
    project: Option<&str>,
) {
    println!(
        "\n{} ({key}): admin {}, generation {}, dollar {}",
        config.label, plan.admin, plan.generation, plan.dollar
    );
    println!("  creations, through the deterministic deployer {CREATE2_DEPLOYER}:");
    for creation in &plan.creations {
        println!(
            "    {:<26} {}  salt {}",
            creation.name, creation.address, creation.salt
        );
        if let Some(said) = plan.described.get(creation.name) {
            println!("      {said}");
        }
        if let Some(arguments) = constructor_arguments(artifacts, creation) {
            for (index, word) in arguments.chunks(32).enumerate() {
                println!("      arg[{index}] 0x{}", alloy::hex::encode(word));
            }
        }
    }
    println!("  the admin's steps, for the hardware key:");
    for step in admin_steps(plan, project) {
        println!("    to {} data {}  ({})", step.to, step.data, step.purpose);
    }
    if project.is_none() {
        println!("    (no project: PROJECT=<the operator's Google Cloud project> proposes it)");
    }
    if hyperliquid::Api::for_chain(plan.chain_id).is_some() {
        println!(
            "  before the send: the deployer must exist on HyperCore (the owner sends it a little \
             USDC or HYPE there); the deploy then puts its transactions into big blocks"
        );
    }
}

/// A creation's constructor arguments: its code past the build's own
/// (linking a library changes no length).
fn constructor_arguments(artifacts: &Path, creation: &Creation) -> Option<Vec<u8>> {
    let path = artifacts
        .join(format!("{}.sol", creation.name))
        .join(format!("{}.json", creation.name));
    let artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let built = artifact["bytecode"]["object"]
        .as_str()?
        .trim_start_matches("0x")
        .len()
        / 2;
    creation.code.get(built..).map(<[u8]>::to_vec)
}

/// On a fork, give the deployer gas money: a fork's accounts hold what the
/// mainnet's do.
async fn fund_on_fork(client: &Client, who: Address, label: &str) -> Result<()> {
    let held = client.balance(who).await?;
    let wanted = U256::from(10u64).pow(U256::from(21u64));
    if held < wanted {
        client
            .raw_request(
                "anvil_setBalance",
                serde_json::json!([who, format!("{wanted:#x}")]),
            )
            .await
            .context("funding the deployer on the fork")?;
        tracing::info!(%who, label, "the deployer is funded on the fork");
    }
    Ok(())
}

/// On HyperEVM, the deployer's transactions go into big blocks (some
/// creations take more than a small block's 2M gas). That needs the
/// deployer to exist on HyperCore: the owner's step, a little USDC or HYPE
/// sent to it there. On a fork there are no big blocks (and nothing is sent
/// to HyperCore): skipped.
async fn big_blocks(mode: Mode, deployer: &PrivateKeySigner, label: &str) -> Result<()> {
    match mode {
        Mode::DryRun => Ok(()),
        Mode::Fork => {
            println!(
                "{label}: a fork has no big blocks; the switch on HyperCore is skipped (the fork \
                 takes every creation in one block)"
            );
            Ok(())
        }
        Mode::Send => {
            let api = hyperliquid::Api::new(hyperliquid::Network::Mainnet);
            if !api.account_exists(deployer.address()).await? {
                bail!(
                    "the deployer {} does not exist on HyperCore, so its transactions can't go \
                     into big blocks: send it a little USDC or HYPE on HyperCore first (the \
                     owner's step), then run the deploy again. Nothing was sent.",
                    deployer.address()
                );
            }
            hyperliquid::HyperCore::new(api, deployer.clone())
                .use_big_blocks(true)
                .await
                .context("switching the deployer to big blocks")?;
            println!("{label}: the deployer's transactions go into big blocks");
            Ok(())
        }
    }
}

/// HyperCore's read precompiles the Hyperliquid driver's constructor asks
/// (a perp's and a spot pair's info, and the pair's token's), as a fork
/// can't answer them: each is given code answering what the public endpoint
/// answers for that one question. A fork's rehearsal alone; reads only
/// from the public endpoint.
async fn hypercore_reads_on_fork(
    config: &ChainConfig,
    venues: &Venues,
    chain_id: u64,
) -> Result<()> {
    alloy::sol! {
        struct ForkSpotInfo {
            string name;
            uint64[2] tokens;
        }
    }
    let Some(hyperliquid) = venues.chain(chain_id)?.hyperliquid.as_ref() else {
        return Ok(());
    };
    if hyperliquid.perps.len() != 1 || hyperliquid.spot_pairs.len() != 1 {
        bail!("the fork's stand-ins answer one perp and one spot pair");
    }
    let http = reqwest::Client::new();
    let public = config
        .public_rpc
        .split_whitespace()
        .next()
        .unwrap_or_default();
    let fork = config.rpc.split_whitespace().next().unwrap_or_default();
    if !mainnet_guard::is_local_url(fork) {
        bail!("{}'s endpoint is not a local fork", config.label);
    }
    let call = |precompile: u16, input: Vec<u8>| {
        let http = http.clone();
        async move {
            let mut to = [0u8; 20];
            to[18..].copy_from_slice(&precompile.to_be_bytes());
            let answer: serde_json::Value = http
                .post(public)
                .json(&serde_json::json!({
                    "jsonrpc": "2.0", "id": 1, "method": "eth_call",
                    "params": [{"to": Address::from(to), "data": Bytes::from(input)}, "latest"],
                }))
                .send()
                .await?
                .json()
                .await?;
            let hex = answer["result"].as_str().ok_or_else(|| {
                anyhow!("HyperCore's precompile {precompile:#x} answered {answer}")
            })?;
            anyhow::Ok((Address::from(to), alloy::hex::decode(hex)?))
        }
    };
    let perp = call(0x080a, U256::from(hyperliquid.perps[0]).abi_encode()).await?;
    let spot = call(0x080b, U256::from(hyperliquid.spot_pairs[0]).abi_encode()).await?;
    let info = ForkSpotInfo::abi_decode(&spot.1).context("reading the spot pair's info")?;
    let token = call(0x080c, U256::from(info.tokens[0]).abi_encode()).await?;
    for (at, answer) in [perp, spot, token] {
        let mut code = Vec::new();
        let length = u16::try_from(answer.len()).context("an answer too long")?;
        // PUSH2 length, PUSH2 15, PUSH1 0, CODECOPY, PUSH2 length, PUSH1 0,
        // RETURN, then the answer: whatever it is asked, it says this.
        code.push(0x61);
        code.extend_from_slice(&length.to_be_bytes());
        code.extend_from_slice(&[0x61, 0x00, 0x0f, 0x60, 0x00, 0x39, 0x61]);
        code.extend_from_slice(&length.to_be_bytes());
        code.extend_from_slice(&[0x60, 0x00, 0xf3]);
        code.extend_from_slice(&answer);
        let answer: serde_json::Value = http
            .post(fork)
            .json(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": "anvil_setCode",
                "params": [at, Bytes::from(code)],
            }))
            .send()
            .await?
            .json()
            .await?;
        if answer.get("error").is_some() {
            bail!("the fork refused a stand-in for {at}: {answer}");
        }
    }
    println!(
        "{}: the fork answers HyperCore's info reads for perp {}, spot pair {} and token {} as \
         the public endpoint does (a fork has no HyperCore)",
        config.label, hyperliquid.perps[0], hyperliquid.spot_pairs[0], info.tokens[0]
    );
    Ok(())
}

// --- Sending the admin's steps ----------------------------------------------

/// Whether the chain shows `step` done: the station factory initialised for
/// this deployment, the driver proposed (trading, or its proposal seen since
/// the deployment began) or the project proposed.
async fn step_done(client: &Client, steps: &AdminSteps, step: &AdminStep) -> Result<bool> {
    match step.kind {
        StepKind::InitialiseStationFactory => {
            let wanted = IMainnetStationFactory::initializeCall::abi_decode(&step.data)?;
            match deploy::station_factory_set(client, step.to).await? {
                None => Ok(false),
                Some(set)
                    if set.attestation == wanted.attestation_
                        && set.asset == wanted.asset_
                        && set.vault_factory == wanted.vaultFactory_
                        && set.implementation == wanted.implementation_ =>
                {
                    Ok(true)
                }
                Some(set) => bail!(
                    "the station factory {} is initialised for another deployment (vault factory {})",
                    step.to,
                    set.vault_factory
                ),
            }
        }
        StepKind::ProposeDriver => {
            let wanted = IMainnetRegistry::proposeDriverCall::abi_decode(&step.data)?;
            let trading = client
                .read(
                    step.to,
                    IMainnetRegistry::tradingDriverCall { name: wanted.name },
                )
                .await?;
            if trading == wanted.implementation {
                return Ok(true);
            }
            use alloy::sol_types::SolEvent;
            let filter = alloy::rpc::types::Filter::new()
                .address(step.to)
                .event_signature(IMainnetRegistry::DriverProposed::SIGNATURE_HASH)
                .topic1(wanted.name)
                .topic2(wanted.implementation.into_word())
                // The block the deploy read before its first send: every
                // proposal lands after it.
                .from_block(steps.from_block + 1);
            Ok(!client
                .logs(&filter)
                .await
                .context("looking for the driver's proposal")?
                .is_empty())
        }
        StepKind::ProposeProject => {
            let asking = TransactionRequest::default()
                .from(steps.admin)
                .to(step.to)
                .input(step.data.clone().into());
            match client.provider().call(asking).await {
                Ok(_) => Ok(false),
                Err(error)
                    if error
                        .as_error_resp()
                        .and_then(|refusal| refusal.as_revert_data())
                        .is_some_and(|data| {
                            use alloy::sol_types::SolError;
                            data.starts_with(&deploy::AlreadyProposed::SELECTOR)
                        }) =>
                {
                    Ok(true)
                }
                Err(error) => {
                    Err(anyhow::Error::new(error).context("asking whether the project is proposed"))
                }
            }
        }
    }
}

/// Send each chain's admin steps not yet done. On a fork, as the admin,
/// impersonated, and with `warp` the fork's clock is then moved past the
/// registry's timelock, so what was proposed is in force. On mainnet
/// nothing is sent: each step's command for the hardware wallet is printed
/// and written beside the steps, for the owner to run.
pub async fn admin(
    chains: Vec<(ChainKey, ChainConfig)>,
    admin_steps_dir: &Path,
    flags: &mainnet_guard::Flags,
    warp: bool,
) -> Result<()> {
    let ids: Vec<u64> = chains
        .iter()
        .filter_map(|(key, _)| key.evm_chain_id())
        .collect();
    // The admin's steps need no generation of their own: the steps file
    // names what they are for.
    let (mode, admin) = mainnet_guard::decide_without_generation(flags, &ids)?;
    if mode == Mode::Send {
        bail!(
            "the admin's steps are never sent from here on mainnet: they are the hardware key's. \
             Run this without DEPLOYER_KEYFILE and CONFIRM_MAINNET to print its commands."
        );
    }
    if warp && mode != Mode::Fork {
        bail!("only a fork's clock can be moved (ON_FORK=1)");
    }
    mainnet_guard::check_admin_steps_dir(mode, admin_steps_dir)?;
    for (key, config) in chains {
        let id = key.evm_chain_id().unwrap_or_default();
        let file = AdminSteps::file_in(admin_steps_dir, id);
        let text = std::fs::read_to_string(&file).with_context(|| {
            format!(
                "reading {}: run the deploy first (task chains:deploy NETWORK=mainnet)",
                file.display()
            )
        })?;
        let steps: AdminSteps = serde_json::from_str(&text)?;
        if steps.admin != admin {
            bail!(
                "{} is prepared for admin {}, not ADMIN={}",
                file.display(),
                steps.admin,
                admin
            );
        }
        // What the hardware key is asked to sign is taken from what the
        // chain answers: without ON_FORK, only the mainnet's endpoints.
        mainnet_guard::check_chain(mode, &config, id).await?;
        let client =
            chain::connect_with(&key, &config, NetworkName::Mainnet, Signing::ReadOnly).await?;
        mainnet_guard::check_chain_id(&config.label, id, client.chain_id)?;
        println!(
            "{} ({key}): {} admin steps",
            config.label,
            steps.steps.len()
        );
        match mode {
            Mode::Fork => {
                send_impersonated(&client, &steps).await?;
                if warp {
                    let timelock = client
                        .read(steps.registry, IMainnetRegistry::timelockCall {})
                        .await?;
                    client
                        .raw_request("evm_increaseTime", serde_json::json!([timelock + 60]))
                        .await?;
                    client
                        .raw_request("evm_mine", serde_json::json!([]))
                        .await?;
                    println!(
                        "  the fork's clock moved {} s, past the registry's timelock",
                        timelock + 60
                    );
                }
            }
            _ => {
                let mut script = String::from("#!/bin/sh\nset -e\n");
                for step in &steps.steps {
                    let done = step_done(&client, &steps, step).await?;
                    let command = ledger_command(step, steps.admin, &config.public_rpc);
                    println!(
                        "  {}: {}\n    {command}",
                        if done { "done" } else { "to send" },
                        step.purpose
                    );
                    if !done {
                        script.push_str(&format!("# {}\n{command}\n", step.purpose));
                    }
                }
                let script_file = file.with_extension("sh");
                std::fs::write(&script_file, script)
                    .with_context(|| format!("writing {}", script_file.display()))?;
                println!(
                    "  nothing was sent: the hardware key's commands are in {}",
                    script_file.display()
                );
            }
        }
    }
    Ok(())
}

async fn send_impersonated(client: &Client, steps: &AdminSteps) -> Result<()> {
    client
        .raw_request("anvil_impersonateAccount", serde_json::json!([steps.admin]))
        .await
        .context("impersonating the admin on the fork")?;
    if client.balance(steps.admin).await? < U256::from(10u64).pow(U256::from(17u64)) {
        client
            .raw_request(
                "anvil_setBalance",
                serde_json::json!([
                    steps.admin,
                    format!("{:#x}", U256::from(10u64).pow(U256::from(19u64)))
                ]),
            )
            .await?;
    }
    for step in &steps.steps {
        if step_done(client, steps, step).await? {
            println!("  done already: {}", step.purpose);
            continue;
        }
        let request = TransactionRequest::default()
            .from(steps.admin)
            .to(step.to)
            .value(step.value)
            .input(step.data.clone().into());
        let receipt = client
            .provider()
            .send_transaction(request)
            .await
            .with_context(|| format!("sending {}", step.purpose))?
            .get_receipt()
            .await
            .with_context(|| format!("waiting for {}", step.purpose))?;
        if !receipt.status() {
            bail!("{} was reverted", step.purpose);
        }
        println!(
            "  sent as the admin (impersonated): {} (gas {})",
            step.purpose, receipt.gas_used
        );
    }
    client
        .raw_request(
            "anvil_stopImpersonatingAccount",
            serde_json::json!([steps.admin]),
        )
        .await?;
    Ok(())
}

// --- The read-back ----------------------------------------------------------

/// One thing the read-back checked.
#[derive(Clone, Debug)]
pub struct Check {
    pub what: String,
    pub ok: bool,
    pub found: String,
}

fn check(checks: &mut Vec<Check>, what: impl Into<String>, ok: bool, found: impl Into<String>) {
    checks.push(Check {
        what: what.into(),
        ok,
        found: found.into(),
    });
}

/// Read every address and setting of each chain's deployment back from the
/// chain, and compare it with the deployment file, the plan this build
/// makes for the file's admin and generation, and `config/mainnet.toml`.
#[allow(clippy::too_many_arguments)]
pub async fn verify(
    chains: Vec<(ChainKey, ChainConfig)>,
    names: &[(String, u64)],
    artifacts: &Path,
    venues: &Venues,
    deployments_dir: &Path,
    flags: &mainnet_guard::Flags,
    project: Option<&str>,
    expect_in_force: bool,
) -> Result<()> {
    let ids: Vec<u64> = chains
        .iter()
        .filter_map(|(key, _)| key.evm_chain_id())
        .collect();
    // ADMIN and GENERATION are required: the read-back checks the chain
    // against them, never the file against itself.
    let decided = mainnet_guard::decide(flags, &ids)?;
    // It sends nothing; without ON_FORK it reads the mainnets alone.
    let mode = if decided.mode == Mode::Fork {
        Mode::Fork
    } else {
        Mode::Send
    };
    let mut failed = Vec::new();
    for (key, config) in chains {
        let id = key.evm_chain_id().unwrap_or_default();
        mainnet_guard::check_chain(mode, &config, id).await?;
        let client =
            chain::connect_with(&key, &config, NetworkName::Mainnet, Signing::ReadOnly).await?;
        mainnet_guard::check_chain_id(&config.label, id, client.chain_id)?;
        let file = Deployments::file_in(deployments_dir, &key);
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let checks = verify_chain(
            &client,
            id,
            &text,
            names,
            artifacts,
            venues,
            decided.admin,
            decided.generation,
            project,
            expect_in_force,
        )
        .await?;
        println!("{} ({key}), from {}:", config.label, file.display());
        for check in &checks {
            println!(
                "  {} {:<58} {}",
                if check.ok { "ok  " } else { "FAIL" },
                check.what,
                check.found
            );
        }
        let bad = checks.iter().filter(|check| !check.ok).count();
        if bad > 0 {
            failed.push(format!("{} ({bad} failed)", config.label));
        }
    }
    if !failed.is_empty() {
        bail!("the read-back failed on {}", failed.join(", "));
    }
    println!("every check passed");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn verify_chain(
    client: &Client,
    chain_id: u64,
    text: &str,
    names: &[(String, u64)],
    artifacts: &Path,
    venues: &Venues,
    admin: Address,
    generation: u64,
    project: Option<&str>,
    expect_in_force: bool,
) -> Result<Vec<Check>> {
    let deployments = Deployments::from_json(text)?;
    let value: serde_json::Value = serde_json::from_str(text)?;
    let record: MainnetRecord = serde_json::from_value(
        value
            .get(MAINNET_KEY)
            .cloned()
            .ok_or_else(|| anyhow!("the deployment file has no mainnet record"))?,
    )?;
    let recorded_generation = deploy::recorded_generation(text);
    let mut checks = Vec::new();
    check(
        &mut checks,
        "the file's admin is ADMIN",
        deployments.roles.admin == admin && deployments.roles.platform == admin,
        format!("{}", deployments.roles.admin),
    );
    check(
        &mut checks,
        format!("the file's generation is {generation}"),
        recorded_generation == Some(generation),
        format!("{recorded_generation:?}"),
    );
    // The plan is made from ADMIN and GENERATION as given, never from the
    // file: every contract below is read from the chain where this build
    // puts it for them.
    let plan = plan_chain(artifacts, venues, chain_id, admin, generation, names)?;
    let p = &plan.platform;

    // Every address the file records is where this build's shipping
    // contract with these arguments lands: the deterministic deployer puts
    // only that creation code there, so no development contract can be at
    // any of them.
    let recorded = [
        ("RiscZeroGroth16Verifier", record.risc_zero, plan.risc_zero),
        (
            "ConfidentialSpaceVerifier",
            deployments.attestation_verifier,
            plan.checker,
        ),
        (
            "AttestationRegistry",
            deployments.attestation,
            p.attestation,
        ),
        ("Publishing", deployments.publishing, p.publishing),
        ("Prices", deployments.prices, p.prices),
        ("StrategyRegistry", deployments.strategies, p.strategies),
        ("ReviewRegistry", deployments.reviews, p.reviews),
        ("AuthorProfiles", deployments.profiles, p.profiles),
        (
            "StationFactory",
            deployments.station_factory,
            p.station_factory,
        ),
        ("Closer", deployments.closer, p.closer),
        ("Vault", record.vault_implementation, p.vault_implementation),
        ("VaultFactory", deployments.factory, p.factory),
        (
            plan.driver.contract,
            record
                .drivers
                .get(&plan.driver.name)
                .copied()
                .unwrap_or_default(),
            plan.driver.address,
        ),
    ];
    for (name, at, planned) in recorded {
        let has_code = client.has_code(at).await?;
        check(
            &mut checks,
            format!("{name}: this build's, with code"),
            at == planned && has_code,
            format!("{at}{}", if has_code { "" } else { " (no code)" }),
        );
    }
    let station_implementation: Option<Address> = value
        .get(deploy::STATION_IMPLEMENTATION_KEY)
        .and_then(|at| serde_json::from_value(at.clone()).ok());
    check(
        &mut checks,
        "Station: this build's, with code",
        station_implementation == Some(p.station_implementation)
            && client.has_code(p.station_implementation).await?,
        format!("{station_implementation:?}"),
    );
    check(
        &mut checks,
        "no stand-in venue, driver or bridge recorded",
        deployments.venue == Address::ZERO
            && deployments.driver == Address::ZERO
            && deployments.bridge == Address::ZERO,
        "",
    );
    check(
        &mut checks,
        "the deployer is not the admin",
        record.deployer != admin,
        format!("deployer {}", record.deployer),
    );
    // What follows reads the contracts where this build puts them for ADMIN
    // and GENERATION: without them there, there is nothing more to read.
    let mut missing = Vec::new();
    for (name, at) in [
        ("AttestationRegistry", p.attestation),
        ("Prices", p.prices),
        ("Publishing", p.publishing),
        ("VaultFactory", p.factory),
        ("StationFactory", p.station_factory),
        (plan.driver.contract, plan.driver.address),
        ("ConfidentialSpaceVerifier", plan.checker),
    ] {
        if !client.has_code(at).await? {
            missing.push(name);
        }
    }
    if !missing.is_empty() {
        check(
            &mut checks,
            "this build's contracts for ADMIN and GENERATION are on the chain",
            false,
            format!("no code at {}", missing.join(", ")),
        );
        return Ok(checks);
    }

    // The dollar.
    let chain = venues.chain(chain_id)?;
    let symbol = client
        .read(chain.dollar, chain::bindings::erc20::IERC20::symbolCall {})
        .await
        .unwrap_or_default();
    check(
        &mut checks,
        format!("the dollar is {} ({})", chain.dollar_symbol, chain.dollar),
        deployments.usd == chain.dollar && symbol == chain.dollar_symbol,
        format!("{} {symbol}", deployments.usd),
    );

    // The registry.
    let registry = p.attestation;
    let read_admin = client
        .read(registry, IMainnetRegistry::adminCall {})
        .await?;
    check(
        &mut checks,
        "registry admin is ADMIN",
        read_admin == admin,
        format!("{read_admin}"),
    );
    let floor = client
        .read(registry, IMainnetRegistry::minTimelockCall {})
        .await?;
    check(
        &mut checks,
        "registry timelock floor is two days",
        floor == MAINNET_MIN_TIMELOCK,
        format!("{floor} s"),
    );
    let timelock = client
        .read(registry, IMainnetRegistry::timelockCall {})
        .await?;
    check(
        &mut checks,
        "registry timelock is two days",
        timelock == MAINNET_MIN_TIMELOCK,
        format!("{timelock} s"),
    );
    let verifiers = client
        .read(registry, IMainnetRegistry::verifiersCall {})
        .await?;
    check(
        &mut checks,
        "the Confidential Space checker is the only checker",
        verifiers == vec![plan.checker],
        format!("{verifiers:?}"),
    );
    let allowed = client
        .read(
            registry,
            IMainnetRegistry::isVerifierAllowedCall {
                verifier: plan.checker,
            },
        )
        .await?;
    check(&mut checks, "the checker is allowed", allowed, "");

    // The checker's pins.
    let image = client
        .read(plan.checker, IMainnetChecker::imageIdCall {})
        .await?;
    check(
        &mut checks,
        "checker pins the guest image id",
        image == GUEST_IMAGE_ID,
        format!("{image}"),
    );
    let trusted = client
        .read(plan.checker, IMainnetChecker::trustedKeysCall {})
        .await?;
    check(
        &mut checks,
        "checker trusts Google's Confidential Space root alone",
        trusted == mainnet_trusted(),
        format!("{trusted:?}"),
    );
    let risc_zero = client
        .read(plan.checker, IMainnetChecker::riscZeroCall {})
        .await?;
    check(
        &mut checks,
        "checker calls our own RISC Zero verifier",
        risc_zero == plan.risc_zero,
        format!("{risc_zero}"),
    );

    // Prices, publishing, the vault factory, the station factory.
    let prices_admin = client.read(p.prices, IMainnetPrices::adminCall {}).await?;
    check(
        &mut checks,
        "prices admin is ADMIN",
        prices_admin == admin,
        format!("{prices_admin}"),
    );
    let notice = client.read(p.prices, IMainnetPrices::noticeCall {}).await?;
    check(
        &mut checks,
        "price list notice is thirty days",
        notice == MAINNET_PRICE_NOTICE,
        format!("{notice} s"),
    );
    let fees_to = client
        .read(p.publishing, IMainnetPublishing::platformCall {})
        .await?;
    check(
        &mut checks,
        "publishing fees go to ADMIN",
        fees_to == admin,
        format!("{fees_to}"),
    );
    let factory_admin = client
        .read(p.factory, IMainnetVaultFactory::adminCall {})
        .await?;
    let factory_platform = client
        .read(p.factory, IMainnetVaultFactory::platformCall {})
        .await?;
    check(
        &mut checks,
        "vault factory admin and platform are ADMIN",
        factory_admin == admin && factory_platform == admin,
        format!("{factory_admin}, {factory_platform}"),
    );
    let initializer = client
        .read(
            p.station_factory,
            IMainnetStationFactory::initializerCall {},
        )
        .await?;
    check(
        &mut checks,
        "station factory is initialised by ADMIN alone",
        initializer == admin,
        format!("{initializer}"),
    );
    let set = deploy::station_factory_set(client, p.station_factory).await?;
    check(
        &mut checks,
        "station factory initialised for this deployment",
        set == Some(wanted_set(&plan)),
        match set {
            Some(_) => "initialised".to_string(),
            None => "not initialised (the admin's step)".to_string(),
        },
    );
    check(
        &mut checks,
        format!("station factory is generation {generation}'s"),
        deployments.station_factory
            == deploy::everywhere_addresses(artifacts, admin, generation)?.station_factory,
        "",
    );

    // The driver.
    let name = deploy::market_id(&plan.driver.name);
    let trading = client
        .read(registry, IMainnetRegistry::tradingDriverCall { name })
        .await?;
    let in_force = trading == plan.driver.address;
    check(
        &mut checks,
        format!(
            "driver {} {}",
            plan.driver.name,
            if expect_in_force {
                "is in force"
            } else {
                "is in force or proposed"
            }
        ),
        in_force || !expect_in_force,
        if in_force {
            "in force".to_string()
        } else {
            format!("the registry trades {trading} (proposed, waiting out the timelock)")
        },
    );
    match (&chain.uniswap, &chain.hyperliquid) {
        (Some(uniswap), _) => {
            let at = plan.driver.address;
            let dollar = client
                .read(at, IMainnetRobinhoodDriver::dollarCall {})
                .await?;
            let router = client
                .read(at, IMainnetRobinhoodDriver::routerCall {})
                .await?;
            let max_age = client
                .read(at, IMainnetRobinhoodDriver::maxFeedAgeCall {})
                .await?;
            let window = client
                .read(at, IMainnetRobinhoodDriver::twapWindowCall {})
                .await?;
            check(
                &mut checks,
                "driver's dollar, router, feed age and TWAP",
                dollar == chain.dollar
                    && router == uniswap.router
                    && max_age == U256::from(uniswap.max_feed_age)
                    && window == uniswap.twap_window,
                format!("{dollar}, {router}, {max_age} s, {window} s"),
            );
            let stocks = client
                .read(at, IMainnetRobinhoodDriver::stocksCall {})
                .await?;
            let listed: Vec<(Address, Address, Address)> = stocks
                .iter()
                .map(|stock| (stock.token, stock.feed, stock.pool))
                .collect();
            let wanted: Vec<(Address, Address, Address)> = uniswap
                .listings
                .iter()
                .map(|listing| (listing.token, listing.feed, listing.pool))
                .collect();
            check(
                &mut checks,
                format!(
                    "driver lists {}",
                    uniswap
                        .listings
                        .iter()
                        .map(|listing| listing.symbol.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                listed == wanted,
                format!("{} listed", listed.len()),
            );
            for listing in &uniswap.listings {
                let max_sale = client
                    .read(
                        at,
                        IMainnetRobinhoodDriver::maxSaleCall {
                            token: listing.token,
                        },
                    )
                    .await?;
                check(
                    &mut checks,
                    format!(
                        "driver's max sale of {} is {}",
                        listing.symbol, listing.max_sale
                    ),
                    max_sale == listing.max_sale,
                    max_sale.to_string(),
                );
            }
        }
        (None, Some(hyperliquid)) => {
            let at = plan.driver.address;
            let usdc = client
                .read(at, IMainnetHyperliquidDriver::usdcCall {})
                .await?;
            let wallet = client
                .read(at, IMainnetHyperliquidDriver::depositWalletCall {})
                .await?;
            check(
                &mut checks,
                "driver's USDC and deposit wallet",
                usdc == chain.dollar && wallet == hyperliquid.deposit_wallet,
                format!("{usdc}, {wallet}"),
            );
            let perps: Vec<u32> = client
                .read(at, IMainnetHyperliquidDriver::perpsCall {})
                .await?
                .iter()
                .map(|perp| perp.index)
                .collect();
            let spots: Vec<u32> = client
                .read(at, IMainnetHyperliquidDriver::spotsCall {})
                .await?
                .iter()
                .map(|spot| spot.pair)
                .collect();
            check(
                &mut checks,
                format!(
                    "driver lists perps {:?} and spot pairs {:?}",
                    hyperliquid.perps, hyperliquid.spot_pairs
                ),
                perps == hyperliquid.perps && spots == hyperliquid.spot_pairs,
                format!("{perps:?}, {spots:?}"),
            );
        }
        _ => {}
    }
    if let Some(project) = project.map(str::trim).filter(|project| !project.is_empty()) {
        let allowed = client
            .read(
                registry,
                IMainnetRegistry::isProjectAllowedCall {
                    projectId: project_hash(project),
                },
            )
            .await?;
        check(
            &mut checks,
            format!(
                "project {project} {}",
                if expect_in_force {
                    "is allowed"
                } else {
                    "(allowed once the timelock passes)"
                }
            ),
            allowed || !expect_in_force,
            if allowed {
                "allowed"
            } else {
                "not yet allowed"
            },
        );
    }
    let no_local_bridge = client
        .read(
            registry,
            IMainnetRegistry::bridgeOfCall {
                name: deploy::market_id("local-bridge"),
            },
        )
        .await?;
    let no_local_driver = client
        .read(
            registry,
            IMainnetRegistry::tradingDriverCall {
                name: deploy::market_id("local"),
            },
        )
        .await?;
    check(
        &mut checks,
        "no stand-in driver or bridge in the registry",
        no_local_bridge == Address::ZERO && no_local_driver == Address::ZERO,
        "",
    );
    Ok(checks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifacts() -> Option<PathBuf> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm/out");
        path.join("ConfidentialSpaceVerifier.sol/ConfidentialSpaceVerifier.json")
            .exists()
            .then_some(path)
    }

    fn venues() -> Venues {
        Venues::read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/mainnet.toml"))
            .unwrap()
    }

    fn names() -> Vec<(String, u64)> {
        vec![
            ("base".to_string(), 8453),
            ("hyperliquid".to_string(), 999),
            ("robinhood".to_string(), 4663),
        ]
    }

    const ADMIN: Address = Address::repeat_byte(0xad);

    #[test]
    fn the_shipping_list_and_the_development_contracts_never_overlap() {
        for name in NEVER_ON_MAINNET {
            assert!(!SHIPPING.contains(&name), "{name}");
        }
    }

    #[test]
    fn the_mainnet_plan_creates_no_development_contract_on_any_chain() {
        let Some(artifacts) = artifacts() else {
            eprintln!("the contracts are not built: skipping");
            return;
        };
        let venues = venues();
        for chain_id in [8453, 4663, 999] {
            let plan = plan_chain(&artifacts, &venues, chain_id, ADMIN, 1, &names()).unwrap();
            for creation in &plan.creations {
                assert!(SHIPPING.contains(&creation.name), "{}", creation.name);
                assert!(
                    !NEVER_ON_MAINNET.contains(&creation.name),
                    "{}",
                    creation.name
                );
                assert!(!creation.name.starts_with("Local"), "{}", creation.name);
            }
            // Its checker is the Confidential Space checker, first and only.
            assert_eq!(
                plan.creations
                    .iter()
                    .find(|creation| creation.address == plan.checker)
                    .map(|creation| creation.name),
                Some("ConfidentialSpaceVerifier")
            );
            assert_eq!(plan.dollar, venues.chain(chain_id).unwrap().dollar);
        }
    }

    #[test]
    fn the_mainnet_plan_names_the_admin_everywhere_and_the_terms_mainnet_takes() {
        let Some(artifacts) = artifacts() else {
            eprintln!("the contracts are not built: skipping");
            return;
        };
        let plan = plan_chain(&artifacts, &venues(), 8453, ADMIN, 1, &names()).unwrap();
        let terms = mainnet_terms();
        assert_eq!(terms.timelock, 2 * 86_400);
        assert_eq!(terms.min_timelock, 2 * 86_400);
        assert_eq!(terms.price_notice, 30 * 86_400);
        let registry = plan
            .creations
            .iter()
            .find(|creation| creation.name == "AttestationRegistry")
            .unwrap();
        let arguments = &registry.code[registry.code.len() - 5 * 32..];
        let (admin, checker, timelock, floor, ttl) =
            <(Address, Address, u64, u64, u64)>::abi_decode_params(arguments).unwrap();
        assert_eq!(
            (admin, checker, timelock, floor, ttl),
            (ADMIN, plan.checker, 2 * 86_400, 2 * 86_400, BLESSING_TTL)
        );
        let steps = admin_steps(&plan, None);
        assert_eq!(
            steps.iter().map(|step| step.kind).collect::<Vec<_>>(),
            vec![StepKind::InitialiseStationFactory, StepKind::ProposeDriver]
        );
        let steps = admin_steps(&plan, Some("shonin-fi"));
        assert_eq!(steps.last().unwrap().kind, StepKind::ProposeProject);
        assert_eq!(
            plan.platform.station_factory,
            deploy::everywhere_addresses(&artifacts, ADMIN, 1)
                .unwrap()
                .station_factory
        );
    }

    #[test]
    fn a_generation_taken_by_another_deployment_is_refused_and_this_one_s_is_carried_on() {
        let wanted = StationFactorySet {
            attestation: Address::repeat_byte(1),
            asset: Address::repeat_byte(2),
            vault_factory: Address::repeat_byte(3),
            implementation: Address::repeat_byte(4),
        };
        assert!(judge_generation("Base", 1, StationFactoryState::Absent, wanted).is_ok());
        assert!(judge_generation("Base", 1, StationFactoryState::Uninitialised, wanted).is_ok());
        assert!(
            judge_generation("Base", 1, StationFactoryState::Initialised(wanted), wanted).is_ok()
        );
        let other = StationFactorySet {
            vault_factory: Address::repeat_byte(9),
            ..wanted
        };
        let error = judge_generation("Base", 1, StationFactoryState::Initialised(other), wanted)
            .unwrap_err()
            .to_string();
        assert!(error.contains("generation 1 is taken on Base"), "{error}");
    }

    #[test]
    fn the_mainnet_venues_list_one_driver_a_chain_under_the_names_drivers_toml_offers_on_mainnet() {
        let venues = venues();
        let drivers: toml::Value = toml::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/drivers.toml"),
            )
            .unwrap(),
        )
        .unwrap();
        for chain_id in [8453, 4663, 999] {
            let chain = venues.chain(chain_id).unwrap();
            let name = chain
                .uniswap
                .as_ref()
                .map(|driver| driver.driver.clone())
                .or_else(|| {
                    chain
                        .hyperliquid
                        .as_ref()
                        .map(|driver| driver.driver.clone())
                })
                .unwrap();
            let networks = drivers["drivers"][name.as_str()]["networks"]
                .as_array()
                .unwrap();
            assert!(
                networks
                    .iter()
                    .any(|network| network.as_str() == Some("mainnet")),
                "{name}"
            );
        }
        let robinhood = venues.chain(4663).unwrap().uniswap.as_ref().unwrap();
        let symbols: Vec<&str> = robinhood
            .listings
            .iter()
            .map(|listing| listing.symbol.as_str())
            .collect();
        assert_eq!(symbols, vec!["NVDA", "AAPL"]);
        let hyperliquid = venues.chain(999).unwrap().hyperliquid.as_ref().unwrap();
        assert_eq!(
            (hyperliquid.perps.clone(), hyperliquid.spot_pairs.clone()),
            (vec![1], vec![107])
        );
    }

    #[test]
    fn each_mainnet_listing_carries_its_max_sale_in_the_token_s_base_units() {
        let venues = venues();
        let sales = |chain_id: u64| -> Vec<(String, U256)> {
            venues
                .chain(chain_id)
                .unwrap()
                .uniswap
                .as_ref()
                .unwrap()
                .listings
                .iter()
                .map(|listing| (listing.symbol.clone(), listing.max_sale))
                .collect()
        };
        let tokens = |whole: u64| U256::from(whole) * U256::from(10u64).pow(U256::from(18u64));
        assert_eq!(sales(8453), vec![("WETH".to_string(), tokens(300))]);
        assert_eq!(
            sales(4663),
            vec![
                ("NVDA".to_string(), tokens(1_000)),
                ("AAPL".to_string(), tokens(100)),
            ]
        );
    }

    #[test]
    fn a_listing_with_no_max_sale_or_a_driver_with_too_many_listings_is_refused() {
        let mut zero = venues();
        zero.evm
            .chains
            .get_mut("4663")
            .unwrap()
            .uniswap
            .as_mut()
            .unwrap()
            .listings[1]
            .max_sale = U256::ZERO;
        assert!(zero.check_listings().is_err());

        let mut three = venues();
        let listings = &mut three
            .evm
            .chains
            .get_mut("4663")
            .unwrap()
            .uniswap
            .as_mut()
            .unwrap()
            .listings;
        listings.push(listings[0].clone());
        assert!(three.check_listings().is_err());

        let mut none = venues();
        none.evm
            .chains
            .get_mut("8453")
            .unwrap()
            .uniswap
            .as_mut()
            .unwrap()
            .listings
            .clear();
        assert!(none.check_listings().is_err());
    }

    #[test]
    fn an_admin_step_s_command_is_for_the_hardware_wallet_as_the_admin() {
        let step = AdminStep {
            chain_id: 8453,
            to: Address::repeat_byte(1),
            data: Bytes::from(vec![0xab, 0xcd]),
            value: U256::ZERO,
            purpose: String::new(),
            kind: StepKind::ProposeDriver,
        };
        let command = ledger_command(&step, ADMIN, "https://mainnet.base.org");
        assert!(
            command.starts_with("cast send --ledger --from "),
            "{command}"
        );
        assert!(command.contains("--chain 8453"), "{command}");
        assert!(command.ends_with(" 0xabcd"), "{command}");
        assert!(!command.contains("private-key"), "{command}");
    }
}
