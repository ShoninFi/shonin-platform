//! Putting the contracts on the chain, once.
//!
//! A deploy is idempotent: if the file is there and the contracts it names are
//! still on the chain, nothing happens. Restarting a service never redeploys
//! anything, and a fresh chain is deployed to on the first service that asks.

use std::path::Path;

use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::sol_types::SolValue;
use anyhow::{Context, Result, anyhow, bail};
use chain::bindings::attestation::{IAttestationRegistry, ImageKind};
use chain::bindings::station::IStationFactory;
use chain::bindings::usd::ILocalUsd;
use chain::bindings::venue::ILocalVenue;
use chain::{ChainKey, Client, Deployments, NetworkName, Role, Roles};

/// How long an image waits between being proposed and becoming blessable, on
/// a local chain. Its clock runs an hour a second, so the full two days pass
/// in under a minute and the timelock's path is exercised every run.
const LOCAL_TIMELOCK: u64 = 2 * 24 * 60 * 60;
/// The shortest the timelock may ever be where depositors' money is at
/// stake. Fixed in the registry when it is deployed, so it can't be lowered
/// later; development chains have none.
const MAINNET_MIN_TIMELOCK: u64 = 2 * 24 * 60 * 60;
/// How long a blessing lasts before an executor has to attest again.
const BLESSING_TTL: u64 = 72 * 60 * 60;
/// What publishing costs, in the asset's own units: a flat dollar, for the
/// review and the build.
const PUBLISH_FEE: u64 = 1_000_000;
/// The least an author may activate a vault with.
const MINIMUM_TANK: u64 = 10_000_000;
/// The least a deposit may be.
const MINIMUM_DEPOSIT: u64 = 10_000_000;
/// The most an unwinder may report as one claim's exit cost: fifty dollars.
const MAX_EXIT_COST: u64 = 50_000_000;
/// The cent every vault mints to the dead address when it opens.
const DEAD_ASSETS: u64 = 10_000;
/// The standard deterministic deployer, present on anvil and on the public
/// chains, which puts the same creation code at the same address everywhere.
const CREATE2_DEPLOYER: Address =
    alloy::primitives::address!("4e59b44847b379578588920ca78fbf26c0b4956c");
/// The salt the station factory and its implementation are deployed with.
const STATION_SALT: B256 = B256::ZERO;
/// How long before a new price list takes effect, on a local chain: the
/// published thirty days, which pass in twelve minutes here.
const LOCAL_PRICE_NOTICE: u64 = 30 * 24 * 60 * 60;
/// What the venue asks a perp position to be backed by.
const MARGIN_BPS: u64 = 1_000;
/// What every account starts with, so nobody has to ask for money first.
const STARTING_BALANCE: u64 = 1_000_000_000_000;

/// The markets the venue lists, and what they start at.
pub const SPOT: &str = "BTC";
pub const PERP: &str = "BTC-PERP";
pub const STARTING_PRICE: u64 = 60_000_000_000;

/// Deploy if nothing is there, and return what is.
pub async fn ensure(
    client: &Client,
    network: NetworkName,
    key: &ChainKey,
    artifacts: &Path,
    file: &Path,
    chains: &[(String, u64)],
) -> Result<Deployments> {
    if file.exists()
        && let Ok(existing) = client.deployments(file).await
        && client.has_code(existing.factory).await.unwrap_or(false)
    {
        tracing::info!(file = %file.display(), "the contracts are already on this chain");
        return Ok(existing);
    }

    let deployments = deploy(client, network, key, artifacts, chains).await?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    std::fs::write(file, deployments.to_json())
        .with_context(|| format!("writing {}", file.display()))?;
    tracing::info!(file = %file.display(), "the addresses are written");
    Ok(deployments)
}

async fn deploy(
    client: &Client,
    network: NetworkName,
    key: &ChainKey,
    artifacts: &Path,
    chains: &[(String, u64)],
) -> Result<Deployments> {
    // None on testnets: nobody else's money is at stake there, and their
    // clocks run in real time, so two days would be two real days. Mainnet
    // gets a floor the admin can never go under.
    let (timelock, min_timelock, price_notice) = match network {
        NetworkName::Local => (LOCAL_TIMELOCK, 0, LOCAL_PRICE_NOTICE),
        NetworkName::Testnet => (0, 0, 0),
        NetworkName::Mainnet => (MAINNET_MIN_TIMELOCK, MAINNET_MIN_TIMELOCK, 0),
    };
    let roles = Roles {
        admin: client.account(Role::Admin)?,
        platform: client.account(Role::Platform)?,
        attestor: client.account(Role::Attestor)?,
        builder: client.account(Role::Builder)?,
        reviewer: client.account(Role::Reviewer)?,
        executor: client.account(Role::Executor)?,
        keeper: client.account(Role::Keeper)?,
        operator: client.account(Role::Operator)?,
        author: client.account(Role::Author)?,
        viewer: client.account(Role::Viewer)?,
    };
    let admin = roles.admin;
    let from_block = client.block_number().await?;

    let usd = put(client, artifacts, admin, "LocalUsd", ().abi_encode_params()).await?;
    let venue = put(
        client,
        artifacts,
        admin,
        "LocalVenue",
        (usd, roles.operator, U256::from(MARGIN_BPS)).abi_encode_params(),
    )
    .await?;
    let verifier = put(
        client,
        artifacts,
        admin,
        "DevAttestationVerifier",
        (roles.attestor,).abi_encode_params(),
    )
    .await?;
    let attestation = put(
        client,
        artifacts,
        admin,
        "AttestationRegistry",
        (admin, verifier, timelock, min_timelock, BLESSING_TTL).abi_encode_params(),
    )
    .await?;
    let publishing = put(
        client,
        artifacts,
        admin,
        "Publishing",
        (usd, roles.platform, U256::from(PUBLISH_FEE)).abi_encode_params(),
    )
    .await?;
    let prices = put(
        client,
        artifacts,
        admin,
        "Prices",
        (
            admin,
            price_notice,
            // The published prices, in dollars scaled by 1e18, in the order
            // `UnitPrices` declares them: a core-hour, a GiB-hour, a GB of
            // egress, a second of inference.
            (
                150_000_000_000_000_000u128,
                10_000_000_000_000_000u128,
                150_000_000_000_000_000u128,
                150_000_000_000_000_000u128 / 3_600,
            ),
        )
            .abi_encode_params(),
    )
    .await?;
    let strategies = put(
        client,
        artifacts,
        admin,
        "StrategyRegistry",
        (attestation, publishing).abi_encode_params(),
    )
    .await?;
    let reviews = put(
        client,
        artifacts,
        admin,
        "ReviewRegistry",
        (attestation, strategies, publishing).abi_encode_params(),
    )
    .await?;
    let profiles = put(
        client,
        artifacts,
        admin,
        "AuthorProfiles",
        ().abi_encode_params(),
    )
    .await?;
    let driver = put(
        client,
        artifacts,
        admin,
        "LocalDriver",
        (venue, usd).abi_encode_params(),
    )
    .await?;
    let bridge = put(
        client,
        artifacts,
        admin,
        "LocalBridge",
        (usd, roles.operator).abi_encode_params(),
    )
    .await?;

    // The station factory and its implementation carry nothing chain-specific
    // in their creation code, so they land at the same addresses on every
    // chain, and so does every vault's station.
    let station_implementation =
        put_everywhere(client, artifacts, admin, "Station", Vec::new()).await?;
    let station_factory = put_everywhere(
        client,
        artifacts,
        admin,
        "StationFactory",
        (admin,).abi_encode_params(),
    )
    .await?;

    // The vault is too large for one contract: its logic is in four
    // libraries linked into it.
    let valuations = put(client, artifacts, admin, "Valuations", Vec::new()).await?;
    let claims = put(client, artifacts, admin, "Claims", Vec::new()).await?;
    let posts = put(client, artifacts, admin, "Posts", Vec::new()).await?;
    let upkeep = put_linked(
        client,
        artifacts,
        admin,
        "Upkeep",
        Vec::new(),
        &[("Valuations", valuations)],
    )
    .await?;
    let vault_implementation = put_linked(
        client,
        artifacts,
        admin,
        "Vault",
        Vec::new(),
        &[
            ("Valuations", valuations),
            ("Upkeep", upkeep),
            ("Claims", claims),
            ("Posts", posts),
        ],
    )
    .await?;

    let names: Vec<(B256, U256)> = chains
        .iter()
        .map(|(name, id)| (name_id(name), U256::from(*id)))
        .collect();
    let factory = put(
        client,
        artifacts,
        admin,
        "VaultFactory",
        (
            // What the factory reads, in the order `VaultFactory.Deps`
            // declares it.
            (
                usd,
                attestation,
                strategies,
                reviews,
                prices,
                station_factory,
                vault_implementation,
                admin,
                roles.platform,
                // The factory this one replaces: none on a fresh deployment.
                Address::ZERO,
            ),
            // The terms every vault is created with, in the order
            // `VaultConfig` declares them.
            (
                100u16,   // the holding fee, a year
                1_000u16, // of new profit, to the author
                500u16,   // of the same profit, to the platform
                10u16,    // on a deposit or a withdrawal, kept in the pool
                100u16,   // the exit band
                U256::from(MINIMUM_DEPOSIT),
                U256::from(MINIMUM_TANK),
                U256::from(DEAD_ASSETS),
                U256::from(MAX_EXIT_COST),
                timers(network),
            ),
            names,
        )
            .abi_encode_params(),
    )
    .await?;

    if client
        .read(station_factory, IStationFactory::implementationCall {})
        .await?
        == Address::ZERO
    {
        client
            .send(
                admin,
                station_factory,
                IStationFactory::initializeCall {
                    attestation_: attestation,
                    asset_: usd,
                    vaultFactory_: factory,
                    implementation_: station_implementation,
                },
            )
            .await?;
    }

    let deployments = Deployments {
        chain_key: key.to_string(),
        chain_id: client.chain_id,
        rpc_url: client.rpc_url.clone(),
        deployed_block: from_block,
        usd,
        venue,
        attestation_verifier: verifier,
        attestation,
        publishing,
        prices,
        strategies,
        reviews,
        profiles,
        factory,
        station_factory,
        driver,
        bridge,
        roles,
    };

    list_markets(client, &deployments).await?;
    propose_images(client, &deployments).await?;
    propose_drivers(client, &deployments).await?;
    hand_out_money(client, &deployments).await?;
    Ok(deployments)
}

/// The thirteen timers, in seconds.
type Timers = (
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
);

/// Every timer a vault runs on, in the order `Timers` declares them. The
/// local chains move an hour per block, so anything a transaction has to beat
/// is stretched to survive a block or two.
fn timers(network: NetworkName) -> Timers {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let local = network == NetworkName::Local;
    (
        10,                                    // slot interval
        5,                                     // slot lead
        3,                                     // slot window
        if local { 2 * HOUR } else { MINUTE }, // post deadline
        if local { 3 * HOUR } else { HOUR },   // transit timeout
        DAY,                                   // fallback after
        if local { DAY } else { 10 * MINUTE }, // usage freshness: many local blocks pass between the executor's signings
        DAY,                                   // tank escape delay
        DAY,                                   // floor delay
        10 * MINUTE,                           // halt grace
        7 * DAY,                               // freeze after
        7 * DAY,                               // wind down
        3 * DAY,                               // activation window
    )
}

/// A chain's name as the factory stores it.
fn name_id(name: &str) -> B256 {
    let mut out = [0u8; 32];
    out[..name.len()].copy_from_slice(name.as_bytes());
    B256::from(out)
}

/// Deploy one contract, from the bytecode the build tool left behind.
async fn put(
    client: &Client,
    artifacts: &Path,
    from: Address,
    name: &str,
    arguments: Vec<u8>,
) -> Result<Address> {
    put_linked(client, artifacts, from, name, arguments, &[]).await
}

/// The same, with the libraries it links to filled in.
async fn put_linked(
    client: &Client,
    artifacts: &Path,
    from: Address,
    name: &str,
    arguments: Vec<u8>,
    libraries: &[(&str, Address)],
) -> Result<Address> {
    let mut bytecode = creation_code(artifacts, name, libraries)?;
    bytecode.extend_from_slice(&arguments);
    let address = client.deploy(from, bytecode).await?;
    tracing::info!(%address, contract = name, "deployed");
    Ok(address)
}

/// Deploy through the deterministic deployer, so the contract has the same
/// address on every chain its creation code is the same on. Already there is
/// already done.
async fn put_everywhere(
    client: &Client,
    artifacts: &Path,
    from: Address,
    name: &str,
    arguments: Vec<u8>,
) -> Result<Address> {
    let mut init = creation_code(artifacts, name, &[])?;
    init.extend_from_slice(&arguments);
    let address = CREATE2_DEPLOYER.create2(STATION_SALT, keccak256(&init));
    if client.has_code(address).await? {
        tracing::info!(%address, contract = name, "already deployed");
        return Ok(address);
    }
    if !client.has_code(CREATE2_DEPLOYER).await? {
        bail!("this chain has no deterministic deployer at {CREATE2_DEPLOYER}");
    }
    let mut data = STATION_SALT.to_vec();
    data.extend_from_slice(&init);
    client
        .send_raw(from, CREATE2_DEPLOYER, Bytes::from(data))
        .await?;
    if !client.has_code(address).await? {
        bail!("{name} did not land at {address}");
    }
    tracing::info!(%address, contract = name, "deployed at its address on every chain");
    Ok(address)
}

/// A contract's creation code, with `libraries` linked in where it calls them.
fn creation_code(artifacts: &Path, name: &str, libraries: &[(&str, Address)]) -> Result<Vec<u8>> {
    let path = artifacts
        .join(format!("{name}.sol"))
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading {}. Run the contracts' build first: `task contracts:build`",
            path.display()
        )
    })?;
    let artifact: serde_json::Value =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let mut hex = artifact["bytecode"]["object"]
        .as_str()
        .ok_or_else(|| anyhow!("{name} has no bytecode"))?
        .trim_start_matches("0x")
        .to_string();
    // Each place a library is called holds a placeholder the build tool
    // wrote; the library's address goes there.
    if let Some(references) = artifact["bytecode"]["linkReferences"].as_object() {
        for libraries_in_file in references.values() {
            let Some(libraries_in_file) = libraries_in_file.as_object() else {
                continue;
            };
            for (library, places) in libraries_in_file {
                let address = libraries
                    .iter()
                    .find(|(wanted, _)| wanted == library)
                    .map(|(_, address)| *address)
                    .ok_or_else(|| anyhow!("{name} needs the library {library}"))?;
                let filled = alloy::hex::encode(address);
                for place in places.as_array().into_iter().flatten() {
                    let start = place["start"].as_u64().unwrap_or(0) as usize * 2;
                    hex.replace_range(start..start + 40, &filled);
                }
            }
        }
    }
    alloy::hex::decode(&hex).with_context(|| format!("{name}'s bytecode"))
}

async fn list_markets(client: &Client, deployments: &Deployments) -> Result<()> {
    for (id, is_perp) in [(SPOT, false), (PERP, true)] {
        client
            .send(
                deployments.roles.operator,
                deployments.venue,
                ILocalVenue::listMarketCall {
                    id: market_id(id),
                    isPerp: is_perp,
                    price: U256::from(STARTING_PRICE),
                },
            )
            .await?;
    }
    tracing::info!("the venue has listed its markets");
    Ok(())
}

/// Propose the images the platform's own services run as. The timelock has to
/// pass before any of them can hold a key, which is the point of it.
async fn propose_images(client: &Client, deployments: &Deployments) -> Result<()> {
    for (digest, kind) in [
        (image_digest("executor"), ImageKind::Executor),
        (image_digest("builder"), ImageKind::Builder),
        (image_digest("reviewer"), ImageKind::Reviewer),
        (image_digest("unwinder"), ImageKind::Unwinder),
    ] {
        client
            .send(
                deployments.roles.admin,
                deployments.attestation,
                IAttestationRegistry::proposeImageCall {
                    imageDigest: digest,
                    kind,
                },
            )
            .await?;
    }
    client
        .send(
            deployments.roles.admin,
            deployments.attestation,
            IAttestationRegistry::proposeProjectCall {
                projectId: project_id(),
            },
        )
        .await?;
    tracing::info!("the images are proposed; the timelock is running");
    Ok(())
}

/// Approve the stand-in driver and bridge under the names `shonin.toml`
/// uses. They wait out the timelock like an image.
async fn propose_drivers(client: &Client, deployments: &Deployments) -> Result<()> {
    client
        .send(
            deployments.roles.admin,
            deployments.attestation,
            IAttestationRegistry::proposeDriverCall {
                name: market_id("local"),
                implementation: deployments.driver,
            },
        )
        .await?;
    client
        .send(
            deployments.roles.admin,
            deployments.attestation,
            IAttestationRegistry::proposeBridgeCall {
                name: market_id("local-bridge"),
                implementation: deployments.bridge,
            },
        )
        .await?;
    tracing::info!("the local driver and bridge are proposed");
    Ok(())
}

async fn hand_out_money(client: &Client, deployments: &Deployments) -> Result<()> {
    for who in [
        deployments.roles.author,
        deployments.roles.viewer,
        deployments.roles.keeper,
        deployments.roles.admin,
    ] {
        client
            .send(
                deployments.roles.admin,
                deployments.usd,
                ILocalUsd::mintCall {
                    to: who,
                    amount: U256::from(STARTING_BALANCE),
                },
            )
            .await?;
    }
    tracing::info!("the accounts are funded");
    Ok(())
}

/// A market's name, as the contract stores it.
pub fn market_id(name: &str) -> alloy::primitives::FixedBytes<32> {
    let mut out = [0u8; 32];
    let bytes = name.as_bytes();
    out[..bytes.len()].copy_from_slice(bytes);
    alloy::primitives::FixedBytes(out)
}

/// What a measured image would be, if there were one. On this chain there is
/// no enclave to measure, so each service's name stands for its image, and the
/// registry treats it exactly as it would a real digest.
pub fn image_digest(service: &str) -> alloy::primitives::FixedBytes<32> {
    alloy::primitives::keccak256(format!("shonin-dev-image:{service}"))
}

pub fn project_id() -> alloy::primitives::FixedBytes<32> {
    alloy::primitives::keccak256("shonin-dev-project")
}
