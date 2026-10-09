//! Putting the contracts on the chain, once.
//!
//! A deploy is idempotent: if the file is there and the contracts it names are
//! still on the chain, nothing happens. Restarting a service never redeploys
//! anything, and a fresh chain is deployed to on the first service that asks.
//!
//! Every contract is created through the standard deterministic deployer, a
//! few to a transaction by a batch creator, so each address follows from the
//! build, the roles and the generation before anything is sent, and a chain
//! whose transactions land a minute apart (HyperEVM's big blocks) takes the
//! whole deployment in a handful of blocks. The chains of a network are
//! deployed at once, each in its own order.

use std::path::Path;

use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::sol_types::{SolCall, SolValue};
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
/// The same on a local chain, whose clock runs an hour a second: ninety
/// days, so a blessing lasts over half an hour of real time and the path from
/// a fresh key's blessing to its gas and its approval at the station is never
/// a race against it, while renewals still come round within a run.
const LOCAL_BLESSING_TTL: u64 = 90 * 24 * 60 * 60;
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
/// What a deployment file records the deployment's generation under. It is
/// written beside the addresses (every reader of the file ignores it).
pub const GENERATION_KEY: &str = "generation";
/// How many generations past an initialised station factory's a refusal
/// looks through for a free one to suggest.
const FREE_GENERATION_SEARCH: u64 = 64;

/// The salt the contracts at one address on every chain (the station
/// factory, the station's implementation and its library, the closer) are
/// deployed with: the deployment's generation, as a 32-byte number. A
/// station factory names its one vault factory, dollar and registry for
/// good, so a deployment beside an older one on the same chains needs a
/// generation of its own, the same on every chain of the network so that
/// a vault's stations keep one address across its chains. Generation 0 is
/// the zero salt.
pub fn salt(generation: u64) -> B256 {
    B256::from(U256::from(generation))
}

alloy::sol! {
    /// What a station factory was initialised with.
    interface IStationFactoryState {
        function attestation() external view returns (address);
        function asset() external view returns (address);
        function vaultFactory() external view returns (address);
        function implementation() external view returns (address);
    }
}

/// Where a generation puts the contracts that sit at one address on every
/// chain, for a deployment whose admin is `admin` (the station factory's
/// creation code names it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EverywhereAddresses {
    pub station_closes: Address,
    pub station_implementation: Address,
    pub station_factory: Address,
    pub closer: Address,
}

/// The addresses generation `generation` puts those contracts at, from the
/// built contracts: nothing is asked of a chain.
pub fn everywhere_addresses(
    artifacts: &Path,
    admin: Address,
    generation: u64,
) -> Result<EverywhereAddresses> {
    let at = |name: &str, arguments: Vec<u8>, libraries: &[(&str, Address)]| -> Result<Address> {
        let mut init = creation_code(artifacts, name, libraries)?;
        init.extend_from_slice(&arguments);
        Ok(CREATE2_DEPLOYER.create2(salt(generation), keccak256(&init)))
    };
    let station_closes = at("StationCloses", Vec::new(), &[])?;
    Ok(EverywhereAddresses {
        station_closes,
        station_implementation: at("Station", Vec::new(), &[("StationCloses", station_closes)])?,
        station_factory: at("StationFactory", (admin,).abi_encode_params(), &[])?,
        closer: at("Closer", Vec::new(), &[])?,
    })
}

/// What a station factory on the chain was initialised with: `None` if it
/// isn't there or wasn't initialised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StationFactorySet {
    pub attestation: Address,
    pub asset: Address,
    pub vault_factory: Address,
    pub implementation: Address,
}

pub async fn station_factory_set(
    client: &Client,
    at: Address,
) -> Result<Option<StationFactorySet>> {
    if !client.has_code(at).await? {
        return Ok(None);
    }
    let implementation = client
        .read(at, IStationFactoryState::implementationCall {})
        .await
        .with_context(|| format!("reading the station factory at {at}"))?;
    if implementation == Address::ZERO {
        return Ok(None);
    }
    Ok(Some(StationFactorySet {
        attestation: client
            .read(at, IStationFactoryState::attestationCall {})
            .await?,
        asset: client.read(at, IStationFactoryState::assetCall {}).await?,
        vault_factory: client
            .read(at, IStationFactoryState::vaultFactoryCall {})
            .await?,
        implementation,
    }))
}

/// Refuse, before anything is sent, a fresh deployment whose generation's
/// station factory on this chain is already initialised. A fresh deployment
/// brings a vault factory, dollar and registry of its own, so a station
/// factory already initialised names another deployment's: every
/// activation at the new vault factory would revert. The refusal names
/// what it was initialised for and the next generation free on this chain.
pub async fn refuse_a_used_station_factory(
    client: &Client,
    artifacts: &Path,
    admin: Address,
    generation: u64,
    file: &Path,
) -> Result<()> {
    let at = everywhere_addresses(artifacts, admin, generation)?;
    let Some(set) = station_factory_set(client, at.station_factory).await? else {
        return Ok(());
    };
    // An earlier try of this very deploy, stopped after initialising it:
    // carried on, not refused, if this is the build it was made from. From
    // another build, its contracts would be deployed again and the station
    // factory could serve none of them: refused here, before anything is.
    let earlier = Landed::load(file, generation);
    if earlier.initialised(&set) {
        if earlier.build == Some(build_of(artifacts)?) {
            return Ok(());
        }
        bail!(
            "the station factory generation {generation} puts at {} on chain {} was initialised by \
             an earlier try of this deploy, from another build of the contracts: carried on, it \
             would deploy them again, and the station factory could serve none of them. Nothing \
             was sent. Deploy every chain of the network under a new generation: \
             `task chains:deploy NETWORK=<network> GENERATION=<n>`, the same on every chain.",
            at.station_factory,
            client.chain_id,
        );
    }
    let mut free = None;
    for later in generation + 1..=generation + FREE_GENERATION_SEARCH {
        let later_at = everywhere_addresses(artifacts, admin, later)?;
        if station_factory_set(client, later_at.station_factory)
            .await?
            .is_none()
        {
            free = Some(later);
            break;
        }
    }
    let implementation = if set.implementation == at.station_implementation {
        "the station implementation this build puts there".to_string()
    } else {
        format!(
            "station implementation {} (this build's is {})",
            set.implementation, at.station_implementation
        )
    };
    bail!(
        "the station factory generation {generation} puts at {} on chain {} is already initialised \
         for another deployment: vault factory {}, dollar {}, attestation registry {}, {implementation}. \
         This deployment brings a vault factory, dollar and registry of its own, so no vault of it \
         could activate there. Nothing was sent. Deploy every chain of the network under a new \
         generation{}: `task chains:deploy NETWORK=<network> GENERATION=<n>`, the same on every chain.",
        at.station_factory,
        client.chain_id,
        set.vault_factory,
        set.asset,
        set.attestation,
        match free {
            Some(free) => format!(" ({free} is free on this chain)"),
            None => String::new(),
        }
    )
}

/// What a deployment file records about one chain the deploy keeps as it
/// is.
#[derive(Clone, Debug)]
pub struct Kept {
    pub key: ChainKey,
    /// The generation its file records; `None` for a file written before
    /// files recorded one.
    pub generation: Option<u64>,
    pub admin: Address,
    pub station_factory: Address,
    /// The station implementation its station factory was initialised
    /// with; `None` for a file written before files recorded it.
    pub station_implementation: Option<Address>,
}

impl Kept {
    /// What `file` records, if it is a deployment file.
    pub fn read(key: &ChainKey, file: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(file).ok()?;
        let deployments = Deployments::from_json(&text).ok()?;
        Some(Self {
            key: key.clone(),
            generation: recorded_generation(&text),
            admin: deployments.roles.admin,
            station_factory: deployments.station_factory,
            station_implementation: serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|value| {
                    value
                        .get(STATION_IMPLEMENTATION_KEY)?
                        .as_str()?
                        .parse()
                        .ok()
                }),
        })
    }
}

/// What a deployment file records the station implementation under: with
/// the station factory's address, it fixes where a vault's stations are.
pub const STATION_IMPLEMENTATION_KEY: &str = "station_implementation";

/// Refuse a deploy whose build would put a vault's stations elsewhere than
/// a chain kept under the same generation does. A station's address follows
/// from the station factory's address and the station implementation, and
/// both follow from the build as well as the generation: two chains under
/// one generation from builds whose station differs would give one vault's
/// stations two addresses, and a bridge send to the vault's own station on
/// the other chain would go where no station is. Called before anything is
/// sent, when some chain is to be deployed.
pub fn refuse_another_build(kept: &[Kept], artifacts: &Path, generation: u64) -> Result<()> {
    for kept in kept {
        let expected = everywhere_addresses(artifacts, kept.admin, generation)?;
        if kept.station_factory == expected.station_factory
            && kept.station_implementation == Some(expected.station_implementation)
        {
            continue;
        }
        bail!(
            "{} is kept under generation {generation} with station factory {} and station \
             implementation {}, and this build puts them at {} and {} under it: a vault's stations \
             would not share one address across the network's chains. Nothing was sent. Deploy \
             every chain of the network afresh under a new generation: \
             `task chains:deploy NETWORK=<network> GENERATION=<n>`, the same on every chain.",
            kept.key,
            kept.station_factory,
            kept.station_implementation
                .map_or_else(|| "unrecorded".to_string(), |at| at.to_string()),
            expected.station_factory,
            expected.station_implementation
        );
    }
    Ok(())
}

/// The generation a deployment file's text records, if any.
pub fn recorded_generation(text: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()?
        .get(GENERATION_KEY)?
        .as_u64()
}

/// The generation a deploy of a network's chains runs under: the one asked
/// for, or else the one the chains kept as they are were deployed under (0
/// when none is kept). Every chain of a network is under one generation, so
/// it refuses a chain kept under another, naming it and what to do.
pub fn settle_generation(requested: Option<u64>, kept: &[Kept], artifacts: &Path) -> Result<u64> {
    let recorded: std::collections::BTreeSet<u64> =
        kept.iter().filter_map(|kept| kept.generation).collect();
    if recorded.len() > 1 {
        let each: Vec<String> = kept
            .iter()
            .map(|kept| match kept.generation {
                Some(generation) => format!("{} under {generation}", kept.key),
                None => format!("{} unrecorded", kept.key),
            })
            .collect();
        bail!(
            "the network's chains were deployed under different generations ({}): a vault's \
             stations would not share one address. Delete their deployment files and deploy \
             every chain afresh under one new generation.",
            each.join(", ")
        );
    }
    let generation = requested.or_else(|| recorded.first().copied()).unwrap_or(0);
    for kept in kept {
        match kept.generation {
            Some(theirs) if theirs != generation => bail!(
                "{} is deployed under generation {theirs}, not {generation}: every chain of a \
                 network is under one generation. Pass GENERATION={theirs} to keep it, or delete \
                 its deployment file to deploy every chain afresh under {generation}.",
                kept.key
            ),
            Some(_) => {}
            None => {
                let expected = everywhere_addresses(artifacts, kept.admin, generation)?;
                if expected.station_factory != kept.station_factory {
                    bail!(
                        "{}'s deployment file records no generation, and its station factory {} \
                         is not the one generation {generation} puts at {}: it was deployed under \
                         another generation, or another build of the station factory. Pass the \
                         generation it was deployed under, or delete its deployment file to \
                         deploy every chain afresh.",
                        kept.key,
                        kept.station_factory,
                        expected.station_factory
                    );
                }
            }
        }
    }
    Ok(generation)
}

/// A deployment file's text: the addresses, and the generation.
pub fn deployment_json(deployments: &Deployments, generation: u64) -> Result<String> {
    let mut value = serde_json::to_value(deployments)?;
    if let Some(object) = value.as_object_mut() {
        object.insert(GENERATION_KEY.to_string(), generation.into());
    }
    Ok(serde_json::to_string_pretty(&value)?)
}
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

/// The deployment `file` names, if its contracts are still on the chain: a
/// deploy keeps it as it is. A file that can't be read, or a chain that
/// can't say whether the code is there, stops the deploy: taken as "not
/// deployed", the file would be written afresh and the steps it records as
/// done or sent forgotten, and a step sent but not yet landed sent again.
pub async fn still_there(client: &Client, file: &Path) -> Result<Option<Deployments>> {
    if !file.exists() {
        return Ok(None);
    }
    let existing = client
        .deployments(file)
        .await
        .with_context(|| format!("reading the deployment {}", file.display()))?;
    let there = client
        .has_code(existing.factory)
        .await
        .context("reading whether the deployment's factory is on the chain")?;
    Ok(there.then_some(existing))
}

/// Deploy if nothing is there, under `generation`, and return what is.
pub async fn ensure(
    client: &Client,
    network: NetworkName,
    key: &ChainKey,
    artifacts: &Path,
    file: &Path,
    chains: &[(String, u64)],
    generation: u64,
) -> Result<Deployments> {
    if let Some(existing) = still_there(client, file).await? {
        let recorded = std::fs::read_to_string(file)
            .ok()
            .and_then(|text| recorded_generation(&text));
        if let Some(recorded) = recorded
            && recorded != generation
        {
            bail!(
                "{key} is deployed under generation {recorded}, not {generation}: every chain of \
                 a network is under one generation"
            );
        }
        // A deploy that stopped part-way through its setup carries on.
        if let Some(setup) = std::fs::read_to_string(file)
            .ok()
            .and_then(|text| recorded_setup(&text))
        {
            tracing::info!(file = %file.display(), done = setup.done.len(), "the contracts are on this chain; finishing their setup");
            let written = Written::read(client, &existing, recorded.unwrap_or(generation)).await?;
            set_up(client, &existing, file, &written, setup).await?;
            return Ok(existing);
        }
        tracing::info!(file = %file.display(), "the contracts are already on this chain");
        return Ok(existing);
    }

    // The addresses are written as soon as the station factory is
    // initialised for them, before the setup's sends, so a deploy that
    // stops in its setup is carried on rather than deployed again (which
    // would need a new generation).
    let deployments = deploy(client, network, key, artifacts, chains, generation, file).await?;
    let written = Written::read(client, &deployments, generation).await?;
    write_file(file, &deployments, &written, Some(&Setup::default()))?;
    Landed::finished(file);
    tracing::info!(file = %file.display(), generation, "the addresses are written");
    set_up(client, &deployments, file, &written, Setup::default()).await?;
    Ok(deployments)
}

/// Deploy to each of `chains` (its key, client, deployment file, and
/// whether it is kept as it is) at once, under `generation`: each chain's
/// own sends stay in their order, and no chain waits for another. Every
/// chain is carried as far as it goes before an error is returned, naming
/// each chain that stopped, so a failure on one never leaves another
/// stopped part-way. Says how long each took.
pub async fn ensure_each(
    chains: &[(ChainKey, Client, std::path::PathBuf, bool)],
    network: NetworkName,
    artifacts: &Path,
    names: &[(String, u64)],
    generation: u64,
) -> Result<()> {
    use tracing::Instrument;
    let started = std::time::Instant::now();
    let each = chains.iter().map(|(key, client, file, _)| {
        let span = tracing::info_span!("deploy", chain = %key);
        async move {
            let began = std::time::Instant::now();
            let result = ensure(client, network, key, artifacts, file, names, generation).await;
            (key, began.elapsed(), result)
        }
        .instrument(span)
    });
    let mut stopped = Vec::new();
    for (key, took, result) in futures::future::join_all(each).await {
        match result {
            Ok(_) => println!("{key}: deployed and set up in {:.0} s", took.as_secs_f64()),
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
            chains.len(),
            stopped.join("; ")
        );
    }
    Ok(())
}

/// What a deployment file records beside the addresses: the generation,
/// and the station implementation its station factory was initialised
/// with, read back from the chain.
struct Written {
    generation: u64,
    station_implementation: Address,
}

impl Written {
    async fn read(client: &Client, deployments: &Deployments, generation: u64) -> Result<Self> {
        let set = station_factory_set(client, deployments.station_factory)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "the station factory at {} is not initialised",
                    deployments.station_factory
                )
            })?;
        Ok(Self {
            generation,
            station_implementation: set.implementation,
        })
    }
}

async fn deploy(
    client: &Client,
    network: NetworkName,
    key: &ChainKey,
    artifacts: &Path,
    chains: &[(String, u64)],
    generation: u64,
    file: &Path,
) -> Result<Deployments> {
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
    // Before anything is sent: a station factory this generation already
    // gave another deployment can never serve this one.
    refuse_a_used_station_factory(client, artifacts, admin, generation, file).await?;
    // What an earlier try of this deploy landed before it stopped, and the
    // build this try is made from, recorded before anything is sent.
    let mut landed = Landed::load(file, generation);
    let from_block = landed.first_block(client.block_number().await?);
    landed.build = Some(build_of(artifacts)?);
    landed.save()?;

    // Every address follows from the build, the roles, the network and the
    // generation, so the whole deployment is known before anything is sent,
    // and its contracts go out a few to a transaction.
    let plan = plan(artifacts, network, &roles, chains, generation)?;
    create(client, admin, artifacts, &plan.creations, &mut landed).await?;
    let at = plan.addresses;

    let wanted = StationFactorySet {
        attestation: at.attestation,
        asset: at.usd,
        vault_factory: at.factory,
        implementation: at.station_implementation,
    };
    initialise_station_factory(client, admin, at.station_factory, wanted).await?;

    let deployments = Deployments {
        chain_key: key.to_string(),
        chain_id: client.chain_id,
        // Never the client's endpoints: one set from the environment is
        // usually a paid URL, which carries its key. Nothing reads it.
        rpc_url: String::new(),
        deployed_block: from_block,
        usd: at.usd,
        venue: at.venue,
        attestation_verifier: at.verifier,
        attestation: at.attestation,
        publishing: at.publishing,
        prices: at.prices,
        strategies: at.strategies,
        reviews: at.reviews,
        profiles: at.profiles,
        factory: at.factory,
        station_factory: at.station_factory,
        driver: at.driver,
        bridge: at.bridge,
        closer: at.closer,
        roles,
    };

    Ok(deployments)
}

/// One contract a deployment creates, through the deterministic deployer:
/// its creation code (the build's, with its libraries linked and its
/// constructor's arguments appended), its salt, and so its address.
#[derive(Clone, Debug)]
struct Creation {
    name: &'static str,
    salt: B256,
    code: Vec<u8>,
    address: Address,
}

impl Creation {
    fn new(
        artifacts: &Path,
        name: &'static str,
        salt: B256,
        arguments: Vec<u8>,
        libraries: &[(&str, Address)],
    ) -> Result<Self> {
        let mut code = creation_code(artifacts, name, libraries)?;
        code.extend_from_slice(&arguments);
        let address = CREATE2_DEPLOYER.create2(salt, keccak256(&code));
        Ok(Self {
            name,
            salt,
            code,
            address,
        })
    }

    /// More gas than its creation can take, from its size alone: the code
    /// stored at 200 a byte (never more than the creation code), the
    /// creation code carried and hashed, and room for what a constructor
    /// writes. What batches are packed by, before they are measured.
    fn gas_bound(&self) -> u64 {
        60_000 + 230 * self.code.len() as u64 + 600_000
    }
}

/// Where a deployment's contracts are.
#[derive(Clone, Copy, Debug)]
struct Planned {
    usd: Address,
    venue: Address,
    verifier: Address,
    attestation: Address,
    publishing: Address,
    prices: Address,
    strategies: Address,
    reviews: Address,
    profiles: Address,
    driver: Address,
    bridge: Address,
    station_implementation: Address,
    station_factory: Address,
    closer: Address,
    factory: Address,
}

/// Every contract a deployment creates, and where.
struct Plan {
    creations: Vec<Creation>,
    addresses: Planned,
}

/// The salt a deployment's own contracts are created under: its generation
/// and its admin, so no two deployments on a chain share one. (The
/// contracts at one address on every chain use [`salt`] alone, as their
/// creation code names the admin where it matters.)
fn deployment_salt(generation: u64, admin: Address) -> B256 {
    keccak256(("shonin-deployment", U256::from(generation), admin).abi_encode_params())
}

/// The deployment's contracts in the order they are created, each with its
/// address, worked out from the build alone: nothing is asked of a chain.
fn plan(
    artifacts: &Path,
    network: NetworkName,
    roles: &Roles,
    chains: &[(String, u64)],
    generation: u64,
) -> Result<Plan> {
    // None on testnets: nobody else's money is at stake there, and their
    // clocks run in real time, so two days would be two real days. Mainnet
    // gets a floor the admin can never go under.
    let (timelock, min_timelock, price_notice) = match network {
        NetworkName::Local => (LOCAL_TIMELOCK, 0, LOCAL_PRICE_NOTICE),
        NetworkName::Testnet => (0, 0, 0),
        NetworkName::Mainnet => (MAINNET_MIN_TIMELOCK, MAINNET_MIN_TIMELOCK, 0),
    };
    let blessing_ttl = blessing_ttl(network);
    let admin = roles.admin;
    let own = deployment_salt(generation, admin);
    let everywhere = salt(generation);
    let mut creations = Vec::new();
    let mut add = |name: &'static str,
                   salt: B256,
                   arguments: Vec<u8>,
                   libraries: &[(&str, Address)]|
     -> Result<Address> {
        let creation = Creation::new(artifacts, name, salt, arguments, libraries)?;
        let address = creation.address;
        creations.push(creation);
        Ok(address)
    };

    let usd = add("LocalUsd", own, ().abi_encode_params(), &[])?;
    let venue = add(
        "LocalVenue",
        own,
        (usd, roles.operator, U256::from(MARGIN_BPS)).abi_encode_params(),
        &[],
    )?;
    let verifier = add(
        "DevAttestationVerifier",
        own,
        (roles.attestor,).abi_encode_params(),
        &[],
    )?;
    let attestation = add(
        "AttestationRegistry",
        own,
        (admin, verifier, timelock, min_timelock, blessing_ttl).abi_encode_params(),
        &[],
    )?;
    let publishing = add(
        "Publishing",
        own,
        (usd, roles.platform, U256::from(PUBLISH_FEE)).abi_encode_params(),
        &[],
    )?;
    let prices = add(
        "Prices",
        own,
        (
            admin,
            price_notice,
            // The fuel one core runs through in a second, from the same
            // constant the executor bills with.
            shared::manifest::FUEL_PER_CORE_SECOND,
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
        &[],
    )?;
    let strategies = add(
        "StrategyRegistry",
        own,
        (attestation, publishing).abi_encode_params(),
        &[],
    )?;
    let reviews = add(
        "ReviewRegistry",
        own,
        (attestation, strategies, publishing).abi_encode_params(),
        &[],
    )?;
    let profiles = add("AuthorProfiles", own, ().abi_encode_params(), &[])?;
    let driver = add("LocalDriver", own, (venue, usd).abi_encode_params(), &[])?;
    let bridge = add(
        "LocalBridge",
        own,
        (usd, roles.operator).abi_encode_params(),
        &[],
    )?;

    // The station factory and its implementation carry nothing chain-specific
    // in their creation code, so they land at the same addresses on every
    // chain, and so does every vault's station.
    // The station's closes run in a library linked into it, itself at the
    // same address everywhere, so the station's creation code is too.
    // Each is under the deployment's generation.
    let station_closes = add("StationCloses", everywhere, Vec::new(), &[])?;
    let station_implementation = add(
        "Station",
        everywhere,
        Vec::new(),
        &[("StationCloses", station_closes)],
    )?;
    let station_factory = add(
        "StationFactory",
        everywhere,
        (admin,).abi_encode_params(),
        &[],
    )?;
    let closer = add("Closer", everywhere, Vec::new(), &[])?;

    // The vault is too large for one contract: its logic is in four
    // libraries linked into it.
    let valuations = add("Valuations", own, Vec::new(), &[])?;
    let claims = add("Claims", own, Vec::new(), &[])?;
    let posts = add("Posts", own, Vec::new(), &[])?;
    let upkeep = add("Upkeep", own, Vec::new(), &[("Valuations", valuations)])?;
    let vault_implementation = add(
        "Vault",
        own,
        Vec::new(),
        &[
            ("Valuations", valuations),
            ("Upkeep", upkeep),
            ("Claims", claims),
            ("Posts", posts),
        ],
    )?;

    let names: Vec<(B256, U256)> = chains
        .iter()
        .map(|(name, id)| (name_id(name), U256::from(*id)))
        .collect();
    let factory = add(
        "VaultFactory",
        own,
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
        &[],
    )?;

    // Kept in step with what a deployment's generation fixes elsewhere.
    debug_assert_eq!(
        everywhere_addresses(artifacts, admin, generation).ok(),
        Some(EverywhereAddresses {
            station_closes,
            station_implementation,
            station_factory,
            closer,
        })
    );
    Ok(Plan {
        creations,
        addresses: Planned {
            usd,
            venue,
            verifier,
            attestation,
            publishing,
            prices,
            strategies,
            reviews,
            profiles,
            driver,
            bridge,
            station_implementation,
            station_factory,
            closer,
            factory,
        },
    })
}

alloy::sol! {
    /// The batch creator (`contracts/evm/src/deploy/BatchCreate2.sol`).
    interface IBatchCreate2 {
        struct Creation {
            bytes32 salt;
            bytes code;
        }
        function create(Creation[] calldata creations)
            external
            returns (address[] memory created, uint256[] memory gasUsed);
    }
}

/// The salt the batch creator itself is created under: one address on
/// every chain, for every deployment.
const BATCH_CREATOR_SALT: B256 = B256::ZERO;

/// The most creation code one batch carries: under the 128 KB a node's pool
/// takes in one transaction, with room for the call around it.
const BATCH_BYTES: usize = 96_000;

/// The gas a big block holds on HyperEVM.
const HYPEREVM_BIG_BLOCK_GAS: u64 = 30_000_000;

/// The most gas one transaction may carry on this chain: a big block's on
/// HyperEVM (the deployer's transactions go into them), and elsewhere the
/// per-transaction cap the newest chains enforce.
fn transaction_gas_cap(client: &Client) -> u64 {
    if hyperliquid::Api::for_chain(client.chain_id).is_some() {
        HYPEREVM_BIG_BLOCK_GAS
    } else {
        16_777_216
    }
}

/// The gas a creation is packed and sent by: what it was measured to
/// take, with a quarter more, or else its bound.
fn packing_gas(creation: &Creation, measured: Option<u64>) -> u64 {
    match measured {
        Some(used) => used / 4 * 5,
        None => creation.gas_bound(),
    }
}

/// Split `creations` (each with the gas it is packed by) into batches, in
/// order, each within the bytes a transaction may carry and within seven
/// tenths of the gas one may: two batches can then share a block whose
/// gas is the cap.
fn batches(creations: &[(Creation, u64)], gas_cap: u64) -> Vec<Vec<(Creation, u64)>> {
    let budget = gas_cap / 10 * 7;
    let mut out: Vec<Vec<(Creation, u64)>> = Vec::new();
    let (mut bytes, mut gas) = (0usize, 0u64);
    for (creation, needs) in creations {
        let fits = out.last().is_some_and(|batch| {
            !batch.is_empty() && bytes + creation.code.len() <= BATCH_BYTES && gas + needs <= budget
        });
        if !fits {
            out.push(Vec::new());
            (bytes, gas) = (0, 0);
        }
        bytes += creation.code.len();
        gas += needs;
        out.last_mut()
            .expect("a batch was just started")
            .push((creation.clone(), *needs));
    }
    out
}

/// The batch creator's call creating `creations`.
fn batch_call(creations: &[&Creation]) -> Vec<u8> {
    IBatchCreate2::createCall {
        creations: creations
            .iter()
            .map(|creation| IBatchCreate2::Creation {
                salt: creation.salt,
                code: Bytes::from(creation.code.clone()),
            })
            .collect(),
    }
    .abi_encode()
}

/// What creating `creation` takes, asked of the chain without sending it
/// (the creator reports each creation's gas): `None` from an endpoint that
/// won't run so large a call.
async fn measure(
    client: &Client,
    from: Address,
    creator: Address,
    creation: &Creation,
    cap: u64,
) -> Option<u64> {
    use alloy::providers::Provider;
    let asked = alloy::rpc::types::TransactionRequest::default()
        .from(from)
        .to(creator)
        .input(Bytes::from(batch_call(&[creation])).into())
        .gas_limit(cap);
    match client.provider().call(asked).await {
        Ok(returned) => IBatchCreate2::createCall::abi_decode_returns(&returned)
            .ok()
            .and_then(|returned| returned.gasUsed.first().map(|used| used.to::<u64>())),
        Err(error) => {
            tracing::warn!(contract = creation.name, error = %error, "a creation could not be measured: packed by its bound");
            None
        }
    }
}

/// The batch creator's address, and its creation code.
fn batch_creator(artifacts: &Path) -> Result<(Address, Vec<u8>)> {
    let code = creation_code(artifacts, "BatchCreate2", &[])?;
    Ok((
        CREATE2_DEPLOYER.create2(BATCH_CREATOR_SALT, keccak256(&code)),
        code,
    ))
}

/// Wait until a contract's code is seen at `address`: a public RPC is often
/// several nodes behind one name, and the one that answers next may not
/// have the block yet.
async fn wait_for_code(client: &Client, name: &str, address: Address) -> Result<()> {
    for _ in 0..15 {
        if client.has_code(address).await? {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    bail!("{name} did not land at {address}")
}

/// Put the batch creator on the chain if it isn't there, through the
/// deterministic deployer.
async fn ensure_batch_creator(client: &Client, from: Address, artifacts: &Path) -> Result<Address> {
    let (address, code) = batch_creator(artifacts)?;
    if client.has_code(address).await? {
        return Ok(address);
    }
    if !client.has_code(CREATE2_DEPLOYER).await? {
        bail!("this chain has no deterministic deployer at {CREATE2_DEPLOYER}");
    }
    let mut data = BATCH_CREATOR_SALT.to_vec();
    data.extend_from_slice(&code);
    client
        .send_raw_with_gas_limit(
            from,
            CREATE2_DEPLOYER,
            Bytes::from(data),
            creation_gas(client),
        )
        .await
        .context("creating the batch creator")?;
    wait_for_code(client, "the batch creator", address).await?;
    tracing::info!(%address, "the batch creator is on the chain");
    Ok(address)
}

/// Create every contract of `creations` not yet on the chain, a batch to a
/// transaction, the batches sent one after another without waiting between
/// them, then wait for them all. Each batch is measured without sending it
/// first, and sent with that gas and some room. A contract already there
/// (an earlier try's, its address following from its code) is kept.
async fn create(
    client: &Client,
    from: Address,
    artifacts: &Path,
    creations: &[Creation],
    landed: &mut Landed,
) -> Result<()> {
    let there = futures::future::try_join_all(
        creations
            .iter()
            .map(|creation| client.has_code(creation.address)),
    )
    .await?;
    let mut missing = Vec::new();
    for (creation, there) in creations.iter().zip(there) {
        if there {
            tracing::info!(address = %creation.address, contract = creation.name, "already on the chain: kept");
            landed.record(creation.name, creation.address, keccak256(&creation.code))?;
        } else {
            missing.push(creation.clone());
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let creator = ensure_batch_creator(client, from, artifacts).await?;
    let cap = transaction_gas_cap(client);
    let measured = futures::future::join_all(
        missing
            .iter()
            .map(|creation| measure(client, from, creator, creation, cap)),
    )
    .await;
    let packed: Vec<(Creation, u64)> = missing
        .iter()
        .zip(measured)
        .map(|(creation, measured)| (creation.clone(), packing_gas(creation, measured)))
        .collect();
    let mut outgoing = Vec::new();
    for batch in batches(&packed, cap) {
        let creations: Vec<&Creation> = batch.iter().map(|(creation, _)| creation).collect();
        let data = batch_call(&creations);
        let calldata: u64 = data
            .iter()
            .map(|byte| if *byte == 0 { 4 } else { 16 })
            .sum();
        let gas =
            (21_000 + calldata + batch.iter().map(|(_, gas)| gas).sum::<u64>() + 100_000).min(cap);
        tracing::info!(
            contracts = batch.len(),
            bytes = creations
                .iter()
                .map(|creation| creation.code.len())
                .sum::<usize>(),
            gas,
            "a batch of creations"
        );
        outgoing.push(Outgoing {
            name: format!(
                "creating {}",
                creations
                    .iter()
                    .map(|creation| creation.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            to: creator,
            data,
            gas_limit: Some(gas),
        });
    }
    send_together(client, from, &outgoing, |_, _| {}).await?;
    for creation in &missing {
        wait_for_code(client, creation.name, creation.address).await?;
        landed.record(creation.name, creation.address, keccak256(&creation.code))?;
        tracing::info!(address = %creation.address, contract = creation.name, "created");
    }
    Ok(())
}

/// One transaction among several one account sends together.
struct Outgoing {
    /// What it does, for its errors.
    name: String,
    to: Address,
    data: Vec<u8>,
    /// Its gas, or the node's estimate (with a fifth more) when `None`.
    gas_limit: Option<u64>,
}

/// How many of one account's transactions are handed to the chain before
/// they are waited for: a node's pool holds only a few of one account's
/// transactions ahead of the next it can run (HyperEVM's, eight).
const IN_FLIGHT: usize = 8;

/// How long one of them may take to land: a big block on HyperEVM comes
/// about once a minute, and a batch fills one.
const TOGETHER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// How many looks a second apart find a sent transaction unknown to every
/// node before it is taken for dropped: a node behind the one that took it
/// may not have seen it yet, and on HyperEVM a transaction waiting for a big
/// block (about one a minute) is unknown to the nodes until it lands, so this
/// spans three of them.
const DROPPED_AFTER: u32 = 180;

/// The key `from` is signed with here: the role's, derived from the
/// testnet mnemonic, when it is set and derives `from`. `None` leaves the
/// signing to the node, as a development chain does for its own accounts.
fn own_key(from: Address) -> Result<Option<alloy::signers::local::PrivateKeySigner>> {
    let phrase = std::env::var(chain::args::TESTNET_EVM_MNEMONIC).unwrap_or_default();
    if phrase.trim().is_empty() {
        return Ok(None);
    }
    Ok(chain::keys_from_mnemonic(&phrase, chain::ROLE_COUNT)?
        .into_iter()
        .find(|key| key.address() == from))
}

/// Send `outgoing` from `from`, numbered one after another, in waves that
/// one block can hold (at most [`IN_FLIGHT`] transactions, their gas within
/// a block's), each wave's sent before any is waited for, telling `sent`
/// each one's place and hash as soon as a node has it; a wave is waited for
/// until each has landed before the next goes. A chain that lands an
/// account's transactions a block apart (HyperEVM's big blocks, a minute
/// each) takes a wave in one block rather than one a block, and no
/// transaction waits behind one its block had no room for (some nodes drop
/// those). A send refused stops the rest; one that lands reverted, or is
/// dropped, is an error naming it. Nothing else may send from `from`
/// meanwhile: the numbers are counted here.
async fn send_together(
    client: &Client,
    from: Address,
    outgoing: &[Outgoing],
    mut sent: impl FnMut(usize, B256),
) -> Result<()> {
    use alloy::providers::Provider;
    let signing = own_key(from)?
        .map(|key| client.signing_provider(key))
        .transpose()?;
    let reading = client.provider();
    // Each one's gas first, estimated as if it were next, whatever its
    // number: a node may refuse to run a call numbered ahead of the
    // account's count.
    let mut requests = Vec::new();
    for out in outgoing {
        let request = alloy::rpc::types::TransactionRequest::default()
            .from(from)
            .to(out.to)
            .input(Bytes::from(out.data.clone()).into());
        let gas = match out.gas_limit {
            Some(gas) => gas,
            None => {
                reading
                    .estimate_gas(request.clone())
                    .await
                    .with_context(|| format!("estimating {}", out.name))?
                    / 5
                    * 6
            }
        };
        requests.push(request.gas_limit(gas));
    }
    let block_gas = block_gas(client).await?;
    let mut nonce = reading
        .get_transaction_count(from)
        .pending()
        .await
        .context("counting the sender's transactions")?;
    let mut index = 0;
    while index < requests.len() {
        // Fees with room for the base fee to climb while they wait: the
        // blocks they fill raise it, and one priced at the fee of the
        // moment could be passed over. Only what each block asks is paid.
        let fees = reading
            .estimate_eip1559_fees()
            .await
            .context("reading the chain's fees")?;
        let mut waiting = Vec::new();
        let mut gas_in_wave = 0u64;
        while index < requests.len() && waiting.len() < IN_FLIGHT {
            let gas = requests[index].gas.unwrap_or_default();
            if !waiting.is_empty() && gas_in_wave + gas > block_gas {
                break;
            }
            gas_in_wave += gas;
            let out = &outgoing[index];
            let request = requests[index]
                .clone()
                .nonce(nonce)
                .max_fee_per_gas(fees.max_fee_per_gas.saturating_mul(2))
                .max_priority_fee_per_gas(fees.max_priority_fee_per_gas);
            let hash = match &signing {
                Some(signing) => *signing
                    .send_transaction(request)
                    .await
                    .with_context(|| format!("sending {}", out.name))?
                    .tx_hash(),
                None => *reading
                    .send_transaction(request)
                    .await
                    .with_context(|| format!("sending {}", out.name))?
                    .tx_hash(),
            };
            sent(index, hash);
            waiting.push((out, hash));
            nonce += 1;
            index += 1;
        }
        for (out, hash) in waiting {
            landed_together(reading, &out.name, hash).await?;
        }
    }
    Ok(())
}

/// The gas one block holds for an account's transactions: a big block's on
/// HyperEVM (the deployer's go into them), the latest block's elsewhere.
async fn block_gas(client: &Client) -> Result<u64> {
    use alloy::providers::Provider;
    if hyperliquid::Api::for_chain(client.chain_id).is_some() {
        return Ok(HYPEREVM_BIG_BLOCK_GAS);
    }
    let latest = client
        .provider()
        .get_block(alloy::eips::BlockId::latest())
        .await
        .context("reading the latest block")?
        .ok_or_else(|| anyhow!("the chain gave no latest block"))?;
    Ok(latest.header.gas_limit)
}

/// Wait until the transaction `hash` (`name`) has landed, for at most
/// [`TOGETHER_TIMEOUT`]: an error if it lands reverted, or if no node knows
/// it for [`DROPPED_AFTER`] looks in a row.
async fn landed_together(
    reading: &alloy::providers::DynProvider,
    name: &str,
    hash: B256,
) -> Result<()> {
    use alloy::providers::Provider;
    let deadline = tokio::time::Instant::now() + TOGETHER_TIMEOUT;
    let mut unknown = 0;
    loop {
        if let Some(receipt) = reading
            .get_transaction_receipt(hash)
            .await
            .with_context(|| format!("waiting for {name}"))?
        {
            if !receipt.status() {
                bail!("{name} ({hash}) was reverted by the chain");
            }
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "{name} ({hash}) has not landed after {} s: run the deploy again once it has",
                TOGETHER_TIMEOUT.as_secs()
            );
        }
        // A transaction no node knows any more was dropped: it will never
        // land, and the deploy run again sends it afresh.
        if reading
            .get_transaction_by_hash(hash)
            .await
            .with_context(|| format!("waiting for {name}"))?
            .is_none()
        {
            unknown += 1;
            if unknown >= DROPPED_AFTER {
                bail!(
                    "{name} ({hash}) was dropped before it landed: run the deploy again to send it afresh"
                );
            }
        } else {
            unknown = 0;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

/// Initialise a station factory with `wanted`, and read it back: a station
/// factory initialised for anything else (another deployment got there
/// first) is refused, naming each difference, rather than written into a
/// deployment that could never activate a vault.
pub async fn initialise_station_factory(
    client: &Client,
    admin: Address,
    station_factory: Address,
    wanted: StationFactorySet,
) -> Result<()> {
    if station_factory_set(client, station_factory)
        .await?
        .is_none()
    {
        client
            .send(
                admin,
                station_factory,
                IStationFactory::initializeCall {
                    attestation_: wanted.attestation,
                    asset_: wanted.asset,
                    vaultFactory_: wanted.vault_factory,
                    implementation_: wanted.implementation,
                },
            )
            .await
            .context("initialising the station factory")?;
    }
    let set = station_factory_set(client, station_factory)
        .await?
        .ok_or_else(|| anyhow!("the station factory at {station_factory} is not initialised"))?;
    let differences: Vec<String> = [
        ("vault factory", set.vault_factory, wanted.vault_factory),
        ("dollar", set.asset, wanted.asset),
        ("attestation registry", set.attestation, wanted.attestation),
        (
            "station implementation",
            set.implementation,
            wanted.implementation,
        ),
    ]
    .into_iter()
    .filter(|(_, is, want)| is != want)
    .map(|(what, is, want)| format!("{what} {is}, not {want}"))
    .collect();
    if !differences.is_empty() {
        bail!(
            "the station factory at {station_factory} is initialised for another deployment ({}): \
             no vault of this one could activate. Deploy every chain of the network under a new \
             generation: `task chains:deploy NETWORK=<network> GENERATION=<n>`.",
            differences.join("; ")
        );
    }
    Ok(())
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

/// How long a blessing lasts on `network`: stretched on the local chains,
/// whose clock runs an hour a second, and three days everywhere else.
fn blessing_ttl(network: NetworkName) -> u64 {
    match network {
        NetworkName::Local => LOCAL_BLESSING_TTL,
        NetworkName::Testnet | NetworkName::Mainnet => BLESSING_TTL,
    }
}

/// Every timer a vault runs on, in the order `Timers` declares them. The
/// local chains move an hour per block, so anything a transaction has to beat
/// is stretched to survive a block or two.
fn timers(network: NetworkName) -> Timers {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let local = network == NetworkName::Local;
    (
        10,                                     // slot interval
        5,                                      // slot lead
        3,                                      // slot window
        if local { 2 * HOUR } else { MINUTE },  // post deadline
        if local { 3 * HOUR } else { HOUR },    // transit timeout
        DAY,                                    // fallback after
        if local { DAY } else { 10 * MINUTE }, // usage freshness: many local blocks pass between the executor's signings
        DAY,                                   // tank escape delay
        DAY,                                   // floor delay
        10 * MINUTE,                           // halt grace
        if local { 30 * DAY } else { 7 * DAY }, // freeze after: a staged close takes minutes, a week of local blocks
        7 * DAY,                                // wind down
        3 * DAY,                                // activation window
    )
}

/// A chain's name as the factory stores it.
fn name_id(name: &str) -> B256 {
    let mut out = [0u8; 32];
    out[..name.len()].copy_from_slice(name.as_bytes());
    B256::from(out)
}

/// The gas a contract creation is given on HyperEVM, rather than an
/// estimate: an endpoint may estimate against a small block's 2M gas while
/// the deployer's transactions go into big blocks (30M), and refuse a large
/// contract. Unused gas is not paid for.
const HYPEREVM_CREATION_GAS: u64 = 25_000_000;

/// The gas limit a creation on this chain is sent with:
/// [`HYPEREVM_CREATION_GAS`] on HyperEVM, the node's estimate elsewhere.
pub fn creation_gas(client: &Client) -> Option<u64> {
    hyperliquid::Api::for_chain(client.chain_id).map(|_| HYPEREVM_CREATION_GAS)
}

/// The contracts a deploy has landed so far, written beside its deployment
/// file (`<file>.partial.json`) after each batch, with the block the first
/// try began at and the build it was made from. Each contract's address
/// follows from its creation code, its arguments and the salt, so one still
/// on the chain is kept by a deploy run again under the same generation,
/// never created twice; the record is what tells an earlier try of this
/// very deploy from another deployment. It goes once the deployment file
/// is written.
struct Landed {
    path: std::path::PathBuf,
    generation: u64,
    from_block: Option<u64>,
    /// The build the last try was made from ([`build_of`]).
    build: Option<B256>,
    contracts: std::collections::BTreeMap<String, (Address, B256)>,
}

/// Every contract a deploy puts on a chain.
const DEPLOYED: [&str; 21] = [
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

/// Which build of the contracts `artifacts` holds: the hash of every
/// deployed contract's creation code, as the build left it.
fn build_of(artifacts: &Path) -> Result<B256> {
    let mut codes = Vec::new();
    for name in DEPLOYED {
        let path = artifacts
            .join(format!("{name}.sol"))
            .join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let artifact: serde_json::Value =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let code = artifact["bytecode"]["object"]
            .as_str()
            .ok_or_else(|| anyhow!("{name} has no bytecode"))?;
        codes.extend_from_slice(keccak256(code.as_bytes()).as_slice());
    }
    Ok(keccak256(&codes))
}

impl Landed {
    fn path_for(file: &Path) -> std::path::PathBuf {
        file.with_extension("partial.json")
    }

    /// The record for `file` under `generation`; empty if there is none or
    /// it is from another generation.
    fn load(file: &Path, generation: u64) -> Self {
        let path = Self::path_for(file);
        let mut landed = Self {
            path: path.clone(),
            generation,
            from_block: None,
            build: None,
            contracts: Default::default(),
        };
        let Some(value) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        else {
            return landed;
        };
        if value.get(GENERATION_KEY).and_then(|g| g.as_u64()) != Some(generation) {
            return landed;
        }
        landed.from_block = value.get("from_block").and_then(|block| block.as_u64());
        landed.build = value
            .get("build")
            .and_then(|build| build.as_str()?.parse().ok());
        for (name, entry) in value
            .get("contracts")
            .and_then(|contracts| contracts.as_object())
            .into_iter()
            .flatten()
        {
            let address = entry.get("address").and_then(|a| a.as_str()?.parse().ok());
            let init = entry.get("init").and_then(|i| i.as_str()?.parse().ok());
            if let (Some(address), Some(init)) = (address, init) {
                landed.contracts.insert(name.clone(), (address, init));
            }
        }
        landed
    }

    /// The block the first try began at, so the deployment's readers start
    /// early enough; this try's if it is the first.
    fn first_block(&mut self, now: u64) -> u64 {
        *self.from_block.get_or_insert(now)
    }

    /// Whether `set` is what this record's earlier try initialised its
    /// station factory with: its vault factory, dollar and registry.
    fn initialised(&self, set: &StationFactorySet) -> bool {
        let at = |name: &str| self.contracts.get(name).map(|(address, _)| *address);
        at("VaultFactory") == Some(set.vault_factory)
            && at("LocalUsd") == Some(set.asset)
            && at("AttestationRegistry") == Some(set.attestation)
    }

    fn record(&mut self, name: &str, address: Address, init: B256) -> Result<()> {
        self.contracts.insert(name.to_string(), (address, init));
        self.save()
    }

    fn save(&self) -> Result<()> {
        let contracts: serde_json::Map<String, serde_json::Value> = self
            .contracts
            .iter()
            .map(|(name, (address, init))| {
                (
                    name.clone(),
                    serde_json::json!({"address": address, "init": init}),
                )
            })
            .collect();
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            &self.path,
            serde_json::to_string_pretty(&serde_json::json!({
                GENERATION_KEY: self.generation,
                "from_block": self.from_block,
                "build": self.build,
                "contracts": contracts,
            }))?,
        )
        .with_context(|| format!("writing {}", self.path.display()))
    }

    /// The deployment file is written: the record is done with.
    fn finished(file: &Path) {
        let _ = std::fs::remove_file(Self::path_for(file));
    }
}

/// A contract's creation code, with `libraries` linked in where it calls them.
pub(crate) fn creation_code(
    artifacts: &Path,
    name: &str,
    libraries: &[(&str, Address)],
) -> Result<Vec<u8>> {
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

/// What a deployment file records of the setup still to finish: the steps
/// already done. Present only until every step is done.
pub const SETUP_DONE_KEY: &str = "setup_done";
/// What it records of the steps sent and not yet seen to land: each
/// step's transaction hash.
pub const SETUP_SENT_KEY: &str = "setup_sent";

/// How long a deploy waits for a step's earlier send, still known to the
/// chain by its hash, to land before it stops and asks to be run again.
const SENT_STEP_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

/// One send of a deployment's setup, after its contracts are on the chain
/// and the file is written: named, so a deploy that stopped part-way picks
/// up after the last one that landed rather than deploying again.
struct Step {
    name: String,
    from: Address,
    to: Address,
    call: Vec<u8>,
    /// What on the chain shows it done.
    shows: Shows,
}

/// What on the chain shows a setup step done.
enum Shows {
    /// The venue lists the market.
    Market(&'static str),
    /// The registry holds the service's image, proposed.
    Image(&'static str),
    /// The registry refuses the project's proposal again: it has no view of
    /// one still in its timelock.
    Project,
    /// The registry names the stand-in driver, or bridge, under its name:
    /// one still in its timelock is not shown.
    Driver,
    Bridge,
    /// The account holds its starting money of the deployment's dollar,
    /// which nobody but the setup has handed out before.
    Money(Address),
}

/// Every setup step, in order: the venue's markets, the images the
/// platform's services run as (the timelock has to pass before any of them
/// can hold a key, which is the point of it), the stand-in driver and
/// bridge under the names `shonin.toml` uses (they wait out the timelock
/// like an image), and the accounts' starting money.
fn setup_steps(deployments: &Deployments) -> Vec<Step> {
    let roles = &deployments.roles;
    let mut steps = Vec::new();
    for (id, is_perp) in [(SPOT, false), (PERP, true)] {
        steps.push(Step {
            name: format!("market {id}"),
            from: roles.operator,
            to: deployments.venue,
            call: ILocalVenue::listMarketCall {
                id: market_id(id),
                isPerp: is_perp,
                price: U256::from(STARTING_PRICE),
            }
            .abi_encode(),
            shows: Shows::Market(id),
        });
    }
    for (service, kind) in [
        ("executor", ImageKind::Executor),
        ("builder", ImageKind::Builder),
        ("reviewer", ImageKind::Reviewer),
        ("unwinder", ImageKind::Unwinder),
        ("holder", ImageKind::Holder),
    ] {
        steps.push(Step {
            name: format!("image {service}"),
            from: roles.admin,
            to: deployments.attestation,
            call: IAttestationRegistry::proposeImageCall {
                imageDigest: image_digest(service),
                kind,
            }
            .abi_encode(),
            shows: Shows::Image(service),
        });
    }
    steps.push(Step {
        name: "project".to_string(),
        from: roles.admin,
        to: deployments.attestation,
        call: IAttestationRegistry::proposeProjectCall {
            projectId: project_id(),
        }
        .abi_encode(),
        shows: Shows::Project,
    });
    steps.push(Step {
        name: "driver local".to_string(),
        from: roles.admin,
        to: deployments.attestation,
        call: IAttestationRegistry::proposeDriverCall {
            name: market_id("local"),
            implementation: deployments.driver,
        }
        .abi_encode(),
        shows: Shows::Driver,
    });
    steps.push(Step {
        name: "bridge local-bridge".to_string(),
        from: roles.admin,
        to: deployments.attestation,
        call: IAttestationRegistry::proposeBridgeCall {
            name: market_id("local-bridge"),
            implementation: deployments.bridge,
        }
        .abi_encode(),
        shows: Shows::Bridge,
    });
    for who in [roles.author, roles.viewer, roles.keeper, roles.admin] {
        steps.push(Step {
            name: format!("money {who}"),
            from: roles.admin,
            to: deployments.usd,
            call: ILocalUsd::mintCall {
                to: who,
                amount: U256::from(STARTING_BALANCE),
            }
            .abi_encode(),
            shows: Shows::Money(who),
        });
    }
    steps
}

alloy::sol! {
    /// The registry's refusal of a proposal it already holds.
    error AlreadyProposed();
}

/// Whether the chain shows `step` done. A read that fails is an error,
/// never "not done": the step would be sent again.
async fn shown_done(client: &Client, deployments: &Deployments, step: &Step) -> Result<bool> {
    use alloy::sol_types::SolError;
    let registry = deployments.attestation;
    // At the latest block, never the node's pending state: a send still
    // waiting in its pool has not landed.
    let latest = alloy::eips::BlockId::latest();
    Ok(match step.shows {
        Shows::Market(id) => {
            client
                .read_at(
                    deployments.venue,
                    ILocalVenue::marketOfCall {
                        market: market_id(id),
                    },
                    latest,
                )
                .await?
                .id
                != B256::ZERO
        }
        Shows::Image(service) => {
            client
                .read_at(
                    registry,
                    IAttestationRegistry::imageOfCall {
                        imageDigest: image_digest(service),
                    },
                    latest,
                )
                .await?
                .allowedAt
                != 0
        }
        Shows::Project => {
            let proposing = alloy::rpc::types::TransactionRequest::default()
                .from(step.from)
                .to(step.to)
                .input(Bytes::from(step.call.clone()).into());
            match alloy::providers::Provider::call(client.provider(), proposing)
                .block(latest)
                .await
            {
                Ok(_) => false,
                Err(error)
                    if error
                        .as_error_resp()
                        .and_then(|refusal| refusal.as_revert_data())
                        .is_some_and(|data| data.starts_with(&AlreadyProposed::SELECTOR)) =>
                {
                    true
                }
                Err(error) => {
                    return Err(
                        anyhow::Error::new(error).context("asking whether the project is proposed")
                    );
                }
            }
        }
        Shows::Driver => {
            client
                .read_at(
                    registry,
                    IAttestationRegistry::tradingDriverCall {
                        name: market_id("local"),
                    },
                    latest,
                )
                .await?
                == deployments.driver
        }
        Shows::Bridge => {
            client
                .read_at(
                    registry,
                    IAttestationRegistry::bridgeOfCall {
                        name: market_id("local-bridge"),
                    },
                    latest,
                )
                .await?
                == deployments.bridge
        }
        Shows::Money(who) => {
            client
                .read_at(
                    deployments.usd,
                    chain::bindings::erc20::IERC20::balanceOfCall { account: who },
                    latest,
                )
                .await?
                >= U256::from(STARTING_BALANCE)
        }
    })
}

/// Whether a step's earlier send, known by its hash, landed: its receipt
/// says so, or it is waited for while the chain still knows the
/// transaction. A transaction the chain no longer knows never landed, unless
/// its sender `from` still has a send waiting: an endpoint may not see a
/// pool another holds it in, so that stops the step rather than sending it
/// again.
async fn landed_by_hash(client: &Client, hash: B256, from: Address) -> Result<bool> {
    use alloy::providers::Provider;
    let provider = client.provider();
    let deadline = tokio::time::Instant::now() + SENT_STEP_WAIT;
    loop {
        if let Some(receipt) = provider
            .get_transaction_receipt(hash)
            .await
            .context("looking for a step's earlier send")?
        {
            return Ok(receipt.status());
        }
        if provider
            .get_transaction_by_hash(hash)
            .await
            .context("looking for a step's earlier send")?
            .is_none()
        {
            let pending = provider.get_transaction_count(from).pending().await;
            let latest = provider.get_transaction_count(from).latest().await;
            if pending.context("reading the sender's waiting sends")?
                > latest.context("reading the sender's landed sends")?
            {
                bail!(
                    "its earlier send {hash} is not found, but its sender has a send waiting: \
                     run the deploy again once that has landed or been dropped"
                );
            }
            return Ok(false);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "its earlier send {hash} is still waiting to land: run the deploy again once it has"
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// What a deployment file records of its unfinished setup.
#[derive(Clone, Debug, Default)]
pub struct Setup {
    /// The steps done, in order.
    pub done: Vec<String>,
    /// Each step sent and not seen to land, by its transaction's hash.
    pub sent: std::collections::BTreeMap<String, B256>,
}

/// What a deployment file records of its setup, or `None` if its setup is
/// finished (or it is from before files recorded it).
pub fn recorded_setup(text: &str) -> Option<Setup> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let done = value.get(SETUP_DONE_KEY)?.as_array()?;
    Some(Setup {
        done: done
            .iter()
            .filter_map(|name| name.as_str().map(str::to_string))
            .collect(),
        sent: value
            .get(SETUP_SENT_KEY)
            .and_then(|sent| serde_json::from_value(sent.clone()).ok())
            .unwrap_or_default(),
    })
}

/// Write a deployment's file: its addresses, its generation and station
/// implementation, and while its setup is unfinished the steps done and
/// sent.
fn write_file(
    file: &Path,
    deployments: &Deployments,
    written: &Written,
    setup: Option<&Setup>,
) -> Result<()> {
    let mut value: serde_json::Value =
        serde_json::from_str(&deployment_json(deployments, written.generation)?)?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            STATION_IMPLEMENTATION_KEY.to_string(),
            serde_json::json!(written.station_implementation),
        );
        if let Some(setup) = setup {
            object.insert(SETUP_DONE_KEY.to_string(), serde_json::json!(setup.done));
            object.insert(SETUP_SENT_KEY.to_string(), serde_json::json!(setup.sent));
        }
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    std::fs::write(file, serde_json::to_string_pretty(&value)?)
        .with_context(|| format!("writing {}", file.display()))
}

/// Run the setup steps not yet done, recording each in the file once it
/// lands; the file is complete once they all have. Each step is looked for
/// on the chain before it is sent, and its earlier send by its hash, so a
/// step that landed unseen (its receipt not read in time, or the deploy
/// stopped between the send and the record) is recorded, never sent again.
/// The steps left go out together, each sender's numbered one after
/// another and sent before any is waited for, the senders at once: on a
/// chain whose blocks come a minute apart they take a block or two, not one
/// each. A step whose send failed stops the deploy: run it again to carry
/// on.
async fn set_up(
    client: &Client,
    deployments: &Deployments,
    file: &Path,
    written: &Written,
    setup: Setup,
) -> Result<()> {
    let steps: Vec<Step> = setup_steps(deployments)
        .into_iter()
        .filter(|step| !setup.done.contains(&step.name))
        .collect();
    let context = |step: &Step| {
        format!(
            "the deployment's setup step \"{}\"; the addresses are written, so run the deploy \
             again to carry on from this step",
            step.name
        )
    };
    let shown = futures::future::join_all(steps.iter().map(|step| async move {
        shown_done(client, deployments, step)
            .await
            .with_context(|| context(step))
    }))
    .await;
    let setup = std::sync::Mutex::new(setup);
    let record = |setup: &mut Setup| {
        if let Err(error) = write_file(file, deployments, written, Some(setup)) {
            tracing::warn!(
                error = format!("{error:#}"),
                "a setup step could not be recorded"
            );
        }
    };
    let mut left: std::collections::BTreeMap<Address, Vec<&Step>> = Default::default();
    for (step, shown) in steps.iter().zip(shown) {
        let mut done = shown?;
        let earlier = lock(&setup).sent.get(&step.name).copied();
        if !done && let Some(hash) = earlier {
            done = landed_by_hash(client, hash, step.from)
                .await
                .with_context(|| context(step))?;
        }
        if done {
            tracing::info!(step = step.name, "already done on the chain");
            let mut setup = lock(&setup);
            setup.sent.remove(&step.name);
            setup.done.push(step.name.clone());
            write_file(file, deployments, written, Some(&setup))?;
        } else {
            left.entry(step.from).or_default().push(step);
        }
    }
    let senders = left.iter().map(|(from, steps)| {
        let setup = &setup;
        async move {
            let outgoing: Vec<Outgoing> = steps
                .iter()
                .map(|step| Outgoing {
                    name: format!("the setup step \"{}\"", step.name),
                    to: step.to,
                    data: step.call.clone(),
                    gas_limit: None,
                })
                .collect();
            // Its hash is in the file as soon as a node has it, before its
            // receipt is waited for.
            send_together(client, *from, &outgoing, |index, hash| {
                let mut setup = lock(setup);
                setup.sent.insert(steps[index].name.clone(), hash);
                record(&mut setup);
            })
            .await
            .with_context(|| {
                "the deployment's setup; the addresses are written, so run the deploy again to \
                 carry on from the steps not yet done"
                    .to_string()
            })?;
            let mut setup = lock(setup);
            for step in steps {
                tracing::info!(step = step.name, "set up");
                setup.sent.remove(&step.name);
                setup.done.push(step.name.clone());
            }
            write_file(file, deployments, written, Some(&setup))
        }
    });
    for result in futures::future::join_all(senders).await {
        result?;
    }
    write_file(file, deployments, written, None)?;
    tracing::info!(
        "the deployment is set up: markets listed, images and drivers proposed, accounts funded"
    );
    Ok(())
}

/// The setup's record, held a moment by whichever sender is writing it.
fn lock(setup: &std::sync::Mutex<Setup>) -> std::sync::MutexGuard<'_, Setup> {
    setup
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};

    use alloy::providers::Provider;
    use alloy::rpc::types::TransactionRequest;
    use chain::{ChainConfig, ChainKind, Signing};

    fn artifacts() -> Option<std::path::PathBuf> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm/out");
        path.join("StationFactory.sol/StationFactory.json")
            .exists()
            .then_some(path)
    }

    fn key(id: u64) -> ChainKey {
        ChainKey::new(ChainKind::Evm, id.to_string())
    }

    fn kept(id: u64, generation: Option<u64>, station_factory: Address) -> Kept {
        Kept {
            key: key(id),
            generation,
            admin: Address::repeat_byte(0xad),
            station_factory,
            station_implementation: None,
        }
    }

    /// A blessing lasts three days on the public chains, and long enough on
    /// a local chain (an hour a second) to last over half an hour of real
    /// time.
    #[test]
    fn a_blessing_lasts_three_days_on_the_public_chains_and_half_an_hour_of_real_time_locally() {
        assert_eq!(blessing_ttl(NetworkName::Testnet), 72 * 3_600);
        assert_eq!(blessing_ttl(NetworkName::Mainnet), 72 * 3_600);
        assert!(blessing_ttl(NetworkName::Local) / 3_600 >= 30 * 60);
    }

    #[test]
    fn the_generation_is_the_one_asked_for_or_the_kept_chains_or_zero() {
        let nowhere = Path::new("/nowhere");
        assert_eq!(settle_generation(None, &[], nowhere).unwrap(), 0);
        assert_eq!(settle_generation(Some(2), &[], nowhere).unwrap(), 2);
        let kept_under_one = [
            kept(1, Some(1), Address::ZERO),
            kept(2, Some(1), Address::ZERO),
        ];
        assert_eq!(
            settle_generation(None, &kept_under_one, nowhere).unwrap(),
            1
        );
        assert_eq!(
            settle_generation(Some(1), &kept_under_one, nowhere).unwrap(),
            1
        );
    }

    #[test]
    fn a_chain_kept_under_another_generation_than_the_deploy_s_is_refused() {
        let nowhere = Path::new("/nowhere");
        let error = settle_generation(Some(2), &[kept(84532, Some(1), Address::ZERO)], nowhere)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("evm:84532") && error.contains("generation 1"),
            "{error}"
        );
        let error = settle_generation(
            None,
            &[
                kept(1, Some(1), Address::ZERO),
                kept(2, Some(2), Address::ZERO),
            ],
            nowhere,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("different generations"), "{error}");
    }

    #[test]
    fn a_kept_chain_that_records_no_generation_is_judged_by_its_station_factory_s_address() {
        let Some(artifacts) = artifacts() else {
            eprintln!("the contracts are not built: skipping");
            return;
        };
        let admin = Address::repeat_byte(0xad);
        let one = everywhere_addresses(&artifacts, admin, 1).unwrap();
        let zero = everywhere_addresses(&artifacts, admin, 0).unwrap();
        assert_ne!(one.station_factory, zero.station_factory);
        assert_ne!(one.station_implementation, zero.station_implementation);
        let deployed_under_one = [kept(1, None, one.station_factory)];
        assert_eq!(
            settle_generation(Some(1), &deployed_under_one, &artifacts).unwrap(),
            1
        );
        let error = settle_generation(None, &deployed_under_one, &artifacts)
            .unwrap_err()
            .to_string();
        assert!(error.contains("records no generation"), "{error}");
    }

    #[test]
    fn a_deployment_file_records_its_generation_and_still_reads_as_a_deployment() {
        let deployments = Deployments::from_json(
            &serde_json::json!({
                "chain_key": "evm:1", "chain_id": 1, "rpc_url": "http://x", "deployed_block": 1,
                "usd": Address::ZERO, "venue": Address::ZERO, "attestation_verifier": Address::ZERO,
                "attestation": Address::ZERO, "publishing": Address::ZERO, "prices": Address::ZERO,
                "strategies": Address::ZERO, "reviews": Address::ZERO, "profiles": Address::ZERO,
                "factory": Address::ZERO, "station_factory": Address::ZERO, "driver": Address::ZERO,
                "bridge": Address::ZERO,
                "roles": {
                    "admin": Address::ZERO, "platform": Address::ZERO, "attestor": Address::ZERO,
                    "builder": Address::ZERO, "reviewer": Address::ZERO, "executor": Address::ZERO,
                    "keeper": Address::ZERO, "operator": Address::ZERO, "author": Address::ZERO,
                    "viewer": Address::ZERO,
                },
            })
            .to_string(),
        )
        .unwrap();
        let text = deployment_json(&deployments, 7).unwrap();
        assert_eq!(recorded_generation(&text), Some(7));
        assert!(Deployments::from_json(&text).is_ok());
        assert_eq!(recorded_generation(&deployments.to_json()), None);
    }

    /// An anvil on a port of its own, stopped when dropped.
    struct Anvil(Child);

    impl Drop for Anvil {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    async fn anvil() -> Option<(Anvil, Client)> {
        let (anvil, url) = anvil_node()?;
        let client = Client::connect(&config_at(url), Signing::Unlocked)
            .await
            .ok()?;
        Some((anvil, client))
    }

    /// A local chain's config, served at `url`.
    fn config_at(url: String) -> ChainConfig {
        ChainConfig {
            label: "anvil".to_string(),
            rpc: url.clone(),
            ws: None,
            dollar: "USDC".to_string(),
            native_usd: None,
            confirmations: Some(0),
            public_rpc: url,
            logs_rpc: None,
            log_range: None,
        }
    }

    /// An anvil, and the URL it answers at.
    fn anvil_node() -> Option<(Anvil, String)> {
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
        Some((Anvil(child), format!("http://127.0.0.1:{port}")))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_second_deployment_is_refused_under_the_first_s_generation_and_activates_under_a_new_one()
     {
        let Some(artifacts) = artifacts() else {
            eprintln!("the contracts are not built: skipping");
            return;
        };
        let Some((_anvil, client)) = anvil().await else {
            eprintln!("anvil is not installed: skipping");
            return;
        };
        let dir = std::env::temp_dir().join(format!("shonin-generation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let key = key(31337);
        let names = [("base".to_string(), 31337u64)];
        // A testnet's terms: no timelock, so the stand-in driver is approved
        // at once and a station can pin it.
        let network = NetworkName::Testnet;

        let first = ensure(
            &client,
            network,
            &key,
            &artifacts,
            &dir.join("first.json"),
            &names,
            0,
        )
        .await
        .unwrap();

        // A clean redeploy beside it, under the same generation: refused
        // before anything is sent, naming the next free generation.
        let admin = client.account(Role::Admin).unwrap();
        let sent_before = client
            .provider()
            .get_transaction_count(admin)
            .await
            .unwrap();
        let error = ensure(
            &client,
            network,
            &key,
            &artifacts,
            &dir.join("second.json"),
            &names,
            0,
        )
        .await
        .expect_err("a station factory already initialised for the first deployment");
        let error = format!("{error:#}");
        assert!(error.contains("already initialised"), "{error}");
        assert!(error.contains(&format!("{}", first.factory)), "{error}");
        assert!(error.contains("1 is free"), "{error}");
        assert_eq!(
            client
                .provider()
                .get_transaction_count(admin)
                .await
                .unwrap(),
            sent_before,
            "something was sent before the refusal"
        );
        assert!(!dir.join("second.json").exists());

        // Under a new generation: a station factory of its own, initialised
        // for this deployment, which creates the home station its vault
        // factory asks for at activation.
        let second_file = dir.join("second.json");
        let second = ensure(&client, network, &key, &artifacts, &second_file, &names, 1)
            .await
            .unwrap();
        assert_ne!(second.station_factory, first.station_factory);
        assert_eq!(
            recorded_generation(&std::fs::read_to_string(&second_file).unwrap()),
            Some(1)
        );
        let set = station_factory_set(&client, second.station_factory)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(set.vault_factory, second.factory);
        assert_eq!(set.asset, second.usd);
        assert_eq!(set.attestation, second.attestation);

        let create_home = |from: Address| {
            let input = IStationFactory::createHomeCall {
                vault: Address::repeat_byte(0x77),
                bundleHash: B256::repeat_byte(0x01),
                drivers: vec![market_id("local")],
                bridges: Vec::new(),
                chains: vec![U256::from(31337u64)],
                exitBandBps: 100,
            }
            .abi_encode();
            TransactionRequest::default()
                .from(from)
                .to(second.station_factory)
                .input(input.into())
        };
        let station = client
            .provider()
            .call(create_home(second.factory))
            .await
            .expect("the new vault factory's activation creates its home station");
        assert_ne!(
            Address::from_slice(&station[12..32]),
            Address::ZERO,
            "no station was created"
        );
        // The first deployment's vault factory is no vault factory of this
        // station factory's.
        assert!(
            client
                .provider()
                .call(create_home(first.factory))
                .await
                .is_err()
        );

        // The first deployment's file, kept as it is, is under generation 0:
        // a deploy asked for generation 1 beside it is refused.
        let error = ensure(
            &client,
            network,
            &key,
            &artifacts,
            &dir.join("first.json"),
            &names,
            1,
        )
        .await
        .expect_err("the first is kept under generation 0");
        assert!(format!("{error:#}").contains("generation 0"), "{error:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_deploy_stopped_part_way_is_finished_by_the_next_without_deploying_again() {
        let Some(artifacts) = artifacts() else {
            eprintln!("the contracts are not built: skipping");
            return;
        };
        let Some((_anvil, client)) = anvil().await else {
            eprintln!("anvil is not installed: skipping");
            return;
        };
        let dir = std::env::temp_dir().join(format!("shonin-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("evm-31337.json");
        let key = key(31337);
        let names = [("base".to_string(), 31337u64)];
        let network = NetworkName::Testnet;

        // Stopped as a deploy stops after its last contract landed and the
        // station factory was initialised, before the deployment file was
        // written: only the record of what landed is there.
        let deployed = deploy(&client, network, &key, &artifacts, &names, 0, &file)
            .await
            .unwrap();
        assert!(!file.exists());
        assert!(Landed::path_for(&file).exists());

        let finished = ensure(&client, network, &key, &artifacts, &file, &names, 0)
            .await
            .expect("carried on under the same generation, not refused");
        assert_eq!(finished.factory, deployed.factory, "deployed again");
        assert_eq!(finished.usd, deployed.usd, "deployed again");
        assert!(!Landed::path_for(&file).exists(), "the record is kept");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            recorded_setup(&text).is_none(),
            "the setup is not recorded as done"
        );
        assert_eq!(recorded_generation(&text), Some(0));
        let held = client
            .read(
                deployed.usd,
                chain::bindings::erc20::IERC20::balanceOfCall {
                    account: deployed.roles.author,
                },
            )
            .await
            .unwrap();
        assert_eq!(held, U256::from(STARTING_BALANCE), "money handed out once");

        // Finished is finished: nothing more is sent.
        let admin = deployed.roles.admin;
        let sent = client
            .provider()
            .get_transaction_count(admin)
            .await
            .unwrap();
        ensure(&client, network, &key, &artifacts, &file, &names, 0)
            .await
            .unwrap();
        assert_eq!(
            client
                .provider()
                .get_transaction_count(admin)
                .await
                .unwrap(),
            sent
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_deploy_stopped_part_way_and_carried_on_from_another_build_is_refused_before_anything_is_sent()
     {
        let Some(built) = artifacts() else {
            eprintln!("the contracts are not built: skipping");
            return;
        };
        let Some((_anvil, client)) = anvil().await else {
            eprintln!("anvil is not installed: skipping");
            return;
        };
        let dir = std::env::temp_dir().join(format!("shonin-rebuilt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A copy of the build, which then changes as a later build would.
        let artifacts = dir.join("out");
        for name in DEPLOYED.into_iter().chain(["BatchCreate2"]) {
            let into = artifacts.join(format!("{name}.sol"));
            std::fs::create_dir_all(&into).unwrap();
            std::fs::copy(
                built
                    .join(format!("{name}.sol"))
                    .join(format!("{name}.json")),
                into.join(format!("{name}.json")),
            )
            .unwrap();
        }
        let file = dir.join("evm-31337.json");
        let key = key(31337);
        let names = [("base".to_string(), 31337u64)];
        let network = NetworkName::Testnet;

        // Stopped after the station factory was initialised, before the
        // deployment file was written.
        deploy(&client, network, &key, &artifacts, &names, 0, &file)
            .await
            .unwrap();
        let driver = artifacts.join("LocalDriver.sol").join("LocalDriver.json");
        let mut artifact: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&driver).unwrap()).unwrap();
        let code = artifact["bytecode"]["object"].as_str().unwrap().to_string();
        artifact["bytecode"]["object"] = serde_json::json!(format!("{code}00"));
        std::fs::write(&driver, artifact.to_string()).unwrap();

        let admin = client.account(Role::Admin).unwrap();
        let sent = client
            .provider()
            .get_transaction_count(admin)
            .await
            .unwrap();
        let error = ensure(&client, network, &key, &artifacts, &file, &names, 0)
            .await
            .expect_err("carried on from another build");
        assert!(format!("{error:#}").contains("another build"), "{error:#}");
        assert_eq!(
            client
                .provider()
                .get_transaction_count(admin)
                .await
                .unwrap(),
            sent,
            "something was sent before the refusal"
        );
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_batch_keeps_within_the_bytes_a_transaction_carries_and_its_share_of_the_gas() {
        let creation = |bytes: usize| Creation {
            name: "Some",
            salt: B256::ZERO,
            code: vec![1; bytes],
            address: Address::ZERO,
        };
        let packed: Vec<(Creation, u64)> = [40_000, 40_000, 30_000, 10_000, 5_000]
            .into_iter()
            .map(|bytes| (creation(bytes), 2_000_000))
            .collect();
        let made = batches(&packed, 30_000_000);
        let sizes: Vec<usize> = made.iter().map(Vec::len).collect();
        // 80,000 bytes fit; the third would pass 96,000.
        assert_eq!(sizes, vec![2, 3]);
        for batch in &made {
            assert!(batch.iter().map(|(c, _)| c.code.len()).sum::<usize>() <= BATCH_BYTES);
        }
        // By gas: seven tenths of 10M holds three of 2M.
        let made = batches(&packed, 10_000_000);
        assert!(made.iter().all(|batch| batch.len() <= 3));
        assert_eq!(made.iter().map(Vec::len).sum::<usize>(), packed.len());
        // A creation over the budget alone still goes, in a batch of its own.
        let made = batches(&[(creation(10), 50_000_000)], 10_000_000);
        assert_eq!(made.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_contracts_land_where_the_plan_puts_them_a_few_to_a_transaction_and_within_their_bounds()
     {
        let Some(artifacts) = artifacts() else {
            eprintln!("the contracts are not built: skipping");
            return;
        };
        let Some((_anvil, client)) = anvil().await else {
            eprintln!("anvil is not installed: skipping");
            return;
        };
        let dir = std::env::temp_dir().join(format!("shonin-batched-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let names = [("base".to_string(), 31337u64)];
        let network = NetworkName::Testnet;
        let admin = client.account(Role::Admin).unwrap();
        let roles = Roles {
            admin,
            platform: client.account(Role::Platform).unwrap(),
            attestor: client.account(Role::Attestor).unwrap(),
            builder: client.account(Role::Builder).unwrap(),
            reviewer: client.account(Role::Reviewer).unwrap(),
            executor: client.account(Role::Executor).unwrap(),
            keeper: client.account(Role::Keeper).unwrap(),
            operator: client.account(Role::Operator).unwrap(),
            author: client.account(Role::Author).unwrap(),
            viewer: client.account(Role::Viewer).unwrap(),
        };
        let planned = plan(&artifacts, network, &roles, &names, 3).unwrap();

        // Every creation's measured gas is within the bound a batch falls
        // back on when an endpoint won't measure it.
        let (creator, _) = batch_creator(&artifacts).unwrap();
        ensure_batch_creator(&client, admin, &artifacts)
            .await
            .unwrap();
        assert!(client.has_code(creator).await.unwrap());
        for creation in &planned.creations {
            let measured = measure(&client, admin, creator, creation, 30_000_000)
                .await
                .unwrap_or_else(|| panic!("{} could not be measured", creation.name));
            assert!(
                packing_gas(creation, Some(measured)) <= creation.gas_bound(),
                "{} took {measured}, over its bound {}",
                creation.name,
                creation.gas_bound()
            );
        }

        let sent_before = client
            .provider()
            .get_transaction_count(admin)
            .await
            .unwrap();
        let file = dir.join("evm-31337.json");
        let deployed = ensure(&client, network, &key(31337), &artifacts, &file, &names, 3)
            .await
            .unwrap();
        let sent = client
            .provider()
            .get_transaction_count(admin)
            .await
            .unwrap()
            - sent_before;
        // Twenty-one contracts, the station factory's initialisation and
        // twelve setup steps of the admin's, in far fewer transactions than
        // one a creation.
        assert!(sent < 21 + 1 + 12, "the admin sent {sent} transactions");
        for creation in &planned.creations {
            assert!(
                client.has_code(creation.address).await.unwrap(),
                "{} is not at {}",
                creation.name,
                creation.address
            );
        }
        assert_eq!(deployed.factory, planned.addresses.factory);
        assert_eq!(deployed.usd, planned.addresses.usd);
        let everywhere = everywhere_addresses(&artifacts, admin, 3).unwrap();
        assert_eq!(deployed.station_factory, everywhere.station_factory);
        assert_eq!(deployed.closer, everywhere.closer);
        assert_eq!(
            station_factory_set(&client, deployed.station_factory)
                .await
                .unwrap()
                .unwrap()
                .implementation,
            everywhere.station_implementation
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn chains_deploy_at_once_and_a_second_run_sends_nothing() {
        let Some(artifacts) = artifacts() else {
            eprintln!("the contracts are not built: skipping");
            return;
        };
        let (Some((_one, first)), Some((_two, second))) = (anvil().await, anvil().await) else {
            eprintln!("anvil is not installed: skipping");
            return;
        };
        let dir = std::env::temp_dir().join(format!("shonin-at-once-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let names = [("base".to_string(), 31337u64)];
        let chains = vec![
            (
                key(31337),
                first,
                dir.join("one").join("evm-31337.json"),
                false,
            ),
            (
                key(31337),
                second,
                dir.join("two").join("evm-31337.json"),
                false,
            ),
        ];
        ensure_each(&chains, NetworkName::Testnet, &artifacts, &names, 0)
            .await
            .unwrap();
        let mut counts = Vec::new();
        for (_, client, file, _) in &chains {
            assert!(recorded_setup(&std::fs::read_to_string(file).unwrap()).is_none());
            let admin = client.account(Role::Admin).unwrap();
            counts.push(
                client
                    .provider()
                    .get_transaction_count(admin)
                    .await
                    .unwrap(),
            );
        }
        // The same addresses on both: they follow from the build alone.
        let one = Deployments::from_json(&std::fs::read_to_string(&chains[0].2).unwrap()).unwrap();
        let two = Deployments::from_json(&std::fs::read_to_string(&chains[1].2).unwrap()).unwrap();
        assert_eq!(one.factory, two.factory);
        assert_eq!(one.station_factory, two.station_factory);
        ensure_each(&chains, NetworkName::Testnet, &artifacts, &names, 0)
            .await
            .unwrap();
        for ((_, client, _, _), before) in chains.iter().zip(counts) {
            let admin = client.account(Role::Admin).unwrap();
            assert_eq!(
                client
                    .provider()
                    .get_transaction_count(admin)
                    .await
                    .unwrap(),
                before,
                "a second run sent something"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A step's earlier send, known by its hash, that the node doesn't know:
    /// taken as never landed (so the step is sent again) only while its
    /// sender has nothing waiting, its count of sends with the pool's no
    /// higher than its count of landed ones. While the sender has a send
    /// waiting, which may be that very one held in a pool this endpoint
    /// doesn't see, the step stops rather than send it again. A send the
    /// node does know is waited for until it lands.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_noted_send_the_node_does_not_know_is_sent_again_only_while_its_sender_has_nothing_waiting()
     {
        let Some((_anvil, client)) = anvil().await else {
            eprintln!("anvil is not installed: skipping");
            return;
        };
        let provider = client.provider();
        let from = client.account(Role::Admin).unwrap();
        let unknown = B256::repeat_byte(0x42);

        // Nothing waiting: never landed, and that is known at once.
        let started = tokio::time::Instant::now();
        assert!(!landed_by_hash(&client, unknown, from).await.unwrap());
        assert!(started.elapsed() < std::time::Duration::from_secs(30));

        // A send of the sender's waits in the node's pool.
        let _: serde_json::Value = provider
            .raw_request("evm_setAutomine".into(), (false,))
            .await
            .unwrap();
        let waiting = *provider
            .send_transaction(
                TransactionRequest::default()
                    .from(from)
                    .to(Address::repeat_byte(0x01))
                    .value(U256::from(1)),
            )
            .await
            .unwrap()
            .tx_hash();
        let pending = provider
            .get_transaction_count(from)
            .pending()
            .await
            .unwrap();
        let latest = provider.get_transaction_count(from).latest().await.unwrap();
        assert!(
            pending > latest,
            "the node doesn't count the waiting send, so this shows nothing"
        );
        let error = landed_by_hash(&client, unknown, from)
            .await
            .expect_err("taken as never landed while its sender has a send waiting");
        assert!(
            format!("{error:#}").contains("has a send waiting"),
            "{error:#}"
        );

        // The waiting send itself is known: waited for until it lands.
        let mining = {
            let provider = provider.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let _: String = provider.raw_request("evm_mine".into(), ()).await.unwrap();
            })
        };
        assert!(landed_by_hash(&client, waiting, from).await.unwrap());
        mining.await.unwrap();

        // Nothing waiting once more: the unknown send never landed.
        assert!(!landed_by_hash(&client, unknown, from).await.unwrap());
    }

    /// The node reached through a stand-in that forwards everything, but
    /// fails every read of a contract's code while `code_fails` is set, and
    /// counts the sends it is handed.
    struct CodeReads {
        node: String,
        http: reqwest::Client,
        code_fails: std::sync::atomic::AtomicBool,
        sends: std::sync::atomic::AtomicUsize,
    }

    async fn forward(
        axum::extract::State(stand_in): axum::extract::State<std::sync::Arc<CodeReads>>,
        body: axum::body::Bytes,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        use std::sync::atomic::Ordering;
        let request: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        let requests = match &request {
            serde_json::Value::Array(requests) => requests.clone(),
            one => vec![one.clone()],
        };
        let method = |request: &serde_json::Value| {
            request["method"].as_str().unwrap_or_default().to_string()
        };
        for request in &requests {
            if method(request).starts_with("eth_send") {
                stand_in.sends.fetch_add(1, Ordering::SeqCst);
            }
        }
        if stand_in.code_fails.load(Ordering::SeqCst)
            && requests
                .iter()
                .any(|request| method(request) == "eth_getCode")
        {
            let failed = |request: &serde_json::Value| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "error": {"code": -32603, "message": "internal error"},
                })
            };
            let answer = match &request {
                serde_json::Value::Array(requests) => {
                    serde_json::Value::Array(requests.iter().map(failed).collect())
                }
                one => failed(one),
            };
            return axum::Json(answer).into_response();
        }
        let answer = stand_in
            .http
            .post(&stand_in.node)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        ([("content-type", "application/json")], answer).into_response()
    }

    /// A contract whose code can't be read is not taken for one missing: the
    /// step stops with the failed read, and nothing is sent, so nothing
    /// already on the chain is deployed again. Once the code reads again,
    /// the same step creates what is missing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_read_of_a_contract_s_code_stops_the_deploy_step_and_sends_nothing() {
        use std::sync::atomic::Ordering;
        let Some(artifacts) = artifacts() else {
            eprintln!("the contracts are not built: skipping");
            return;
        };
        let Some((_anvil, node_url)) = anvil_node() else {
            eprintln!("anvil is not installed: skipping");
            return;
        };
        let Ok(node) = Client::connect(&config_at(node_url.clone()), Signing::Unlocked).await
        else {
            eprintln!("anvil did not start: skipping");
            return;
        };
        let stand_in = std::sync::Arc::new(CodeReads {
            node: node_url,
            http: reqwest::Client::new(),
            code_fails: Default::default(),
            sends: Default::default(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new()
            .route("/", axum::routing::post(forward))
            .with_state(stand_in.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        let client = Client::connect(
            &config_at(format!("http://127.0.0.1:{port}")),
            Signing::Unlocked,
        )
        .await
        .unwrap();

        let dir = std::env::temp_dir().join(format!("shonin-code-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let admin = client.account(Role::Admin).unwrap();
        let roles = Roles {
            admin,
            platform: client.account(Role::Platform).unwrap(),
            attestor: client.account(Role::Attestor).unwrap(),
            builder: client.account(Role::Builder).unwrap(),
            reviewer: client.account(Role::Reviewer).unwrap(),
            executor: client.account(Role::Executor).unwrap(),
            keeper: client.account(Role::Keeper).unwrap(),
            operator: client.account(Role::Operator).unwrap(),
            author: client.account(Role::Author).unwrap(),
            viewer: client.account(Role::Viewer).unwrap(),
        };
        let names = [("base".to_string(), 31337u64)];
        let planned = plan(&artifacts, NetworkName::Testnet, &roles, &names, 0).unwrap();
        let creations = &planned.creations[..2];
        let file = dir.join("evm-31337.json");
        let mut landed = Landed::load(&file, 0);
        let sent_before = node.provider().get_transaction_count(admin).await.unwrap();

        stand_in.code_fails.store(true, Ordering::SeqCst);
        let error = create(&client, admin, &artifacts, creations, &mut landed)
            .await
            .expect_err("a contract whose code couldn't be read was taken for one missing");
        assert!(
            format!("{error:#}").contains("reading a contract's code"),
            "{error:#}"
        );
        assert_eq!(
            stand_in.sends.load(Ordering::SeqCst),
            0,
            "something was sent"
        );
        assert_eq!(
            node.provider().get_transaction_count(admin).await.unwrap(),
            sent_before,
            "something was sent"
        );
        for creation in creations {
            assert!(!node.has_code(creation.address).await.unwrap());
        }

        stand_in.code_fails.store(false, Ordering::SeqCst);
        create(&client, admin, &artifacts, creations, &mut landed)
            .await
            .unwrap();
        for creation in creations {
            assert!(
                node.has_code(creation.address).await.unwrap(),
                "{} is not at {}",
                creation.name,
                creation.address
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
