//! Hyperliquid vaults on Hyperliquid testnet, whose dollar is HyperEVM's own
//! USDC rather than the stand-in dollar.
//!
//! A testnet generation deployed with the stand-ins put them on HyperEVM like
//! on every chain: a vault there settles in the stand-in dollar, which the
//! venue has never heard of. A vault that trades on Hyperliquid itself needs
//! a factory whose asset is the USDC that HyperCore sends home, a station
//! factory with the same asset, the station built with delayed closes, and
//! the Hyperliquid driver approved.
//!
//! A generation deployed with the chain's own dollar (every public network
//! now) already has the factory and station factory in USDC: then only the
//! driver is deployed and proposed, and the generation's own factory is the
//! stack's, so the services as deployed (the cloud's among them) serve its
//! vaults with nothing put in place. Otherwise this deploys the rest beside
//! the testnet deployment, using its registries, prices and vault
//! implementation:
//!
//! - `HyperliquidDriver`, for the ETH perp and the HYPE/USDC pair (the venue
//!   numbers them per network, so both are read from its API);
//! - a `Station` implementation and a `StationFactory` whose asset is USDC;
//! - a `VaultFactory` whose asset is USDC, with the deployment's factory as
//!   the one it replaces, so a service that keeps the factory it is given
//!   and every one before it keeps both;
//! - the driver proposed to the registry as `hyperliquid` (testnets have no
//!   timelock, so it is approved at once).
//!
//! The addresses go to their own file beside the deployment's
//! (`hyperliquid-<chain id>.json`), and the deployment as the services should
//! read it with this factory in place ([`adopted`]) beside that
//! (`hyperliquid-<chain id>.deployment.json`).
//!
//! It also does the steps of a run that the platform's own flows don't on a
//! testnet: dollars for the author and a depositor on HyperEVM (sent home
//! from the funder's HyperCore account, since HyperEVM's USDC can't be
//! minted), activating with a USDC tank, and the station's HyperCore account
//! funded by a send from the funder (on testnet, deposits from HyperEVM to
//! HyperCore fail for addresses without mainnet history, so the vault's own
//! deposits stay on HyperEVM as cash).
//!
//! Testnet only: it refuses any chain but HyperEVM's testnet.

use std::path::{Path, PathBuf};
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol_types::{SolCall, SolValue};
use anyhow::{Context, Result, anyhow, bail};
use chain::bindings::attestation::IAttestationRegistry;
use chain::bindings::erc20::IERC20;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::vault::{ClaimState, IVault};
use chain::{Client, Deployments, Role};
use hyperliquid::{Api, HyperCore};
use serde::{Deserialize, Serialize};
use serde_json::json;
use shared::Allowances;

/// HyperEVM's testnet: the only chain this deploys to.
pub const CHAIN_ID: u64 = 998;
/// Circle's USDC on HyperEVM testnet, and its deposit contract into
/// HyperCore.
pub const USDC: Address = alloy::primitives::address!("2B3370eE501B4a559b57D449569354196457D8Ab");
pub const CORE_DEPOSIT_WALLET: Address =
    alloy::primitives::address!("0B80659a4076E9E93C7DbE0f10675A16a3e5C206");
/// The perp and the spot token the driver lists, by the venue's names.
pub const PERP: &str = "ETH";
pub const SPOT: &str = "HYPE";
/// The dollar's system address on HyperCore: a spot send of USDC there lands
/// as USDC at the sender's own address on HyperEVM.
const USDC_SYSTEM: Address =
    alloy::primitives::address!("2000000000000000000000000000000000000000");

alloy::sol! {
    interface IFactoryParts {
        function vaultImplementation() external view returns (address);
        function asset() external view returns (address);
        function previous() external view returns (address);
    }
    interface IStationFactoryParts {
        function implementation() external view returns (address);
    }
}

/// What this deploys, as its file records it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HyperliquidVaults {
    pub chain_id: u64,
    /// The block before the first of these landed: where a reader of the
    /// new factory's vaults starts.
    pub deployed_block: u64,
    pub usdc: Address,
    pub core_deposit_wallet: Address,
    pub driver: Address,
    pub station_implementation: Address,
    pub station_factory: Address,
    pub factory: Address,
    /// The deployment's own factory, which this one replaces.
    pub previous_factory: Address,
    /// The generation of the deployment this stack was deployed beside, as
    /// its file records it; `None` for a stack or a deployment from before
    /// deployments recorded one. A stack is only kept beside a deployment of
    /// its own generation.
    #[serde(default)]
    pub generation: Option<u64>,
}

impl HyperliquidVaults {
    /// Where its file goes: beside the deployment's.
    pub fn file_in(deployments_dir: &Path) -> PathBuf {
        deployments_dir.join(format!("hyperliquid-{CHAIN_ID}.json"))
    }
}

/// The deployment as the services should read it once these are in use:
/// the new factory in place of the old one (which it names as the one it
/// replaces, so its vaults are still kept), and its station factory. The
/// stand-in dollar stays the publishing fee's token.
pub fn adopted(deployments: &Deployments, vaults: &HyperliquidVaults) -> Deployments {
    let mut adopted = deployments.clone();
    adopted.factory = vaults.factory;
    adopted.station_factory = vaults.station_factory;
    adopted
}

/// Deploy what is missing, or return what the file names if all of it is on
/// the chain. Testnet's HyperEVM only.
pub async fn ensure(
    client: &Client,
    artifacts: &Path,
    deployments: &Deployments,
    generation: Option<u64>,
    file: &Path,
) -> Result<HyperliquidVaults> {
    if client.chain_id != CHAIN_ID {
        bail!(
            "Hyperliquid vaults with real USDC are deployed on HyperEVM's testnet ({CHAIN_ID}) only, not {}",
            client.chain_id
        );
    }
    if let Ok(text) = std::fs::read_to_string(file) {
        let vaults: HyperliquidVaults =
            serde_json::from_str(&text).with_context(|| format!("reading {}", file.display()))?;
        // The deployment as deployed, or as adopted with this factory in
        // place.
        let mut all_there = (vaults.previous_factory == deployments.factory
            || vaults.factory == deployments.factory)
            && vaults.generation == generation;
        for address in [
            vaults.driver,
            vaults.station_implementation,
            vaults.station_factory,
            vaults.factory,
        ] {
            all_there = all_there && client.has_code(address).await?;
        }
        if all_there {
            tracing::info!(factory = %vaults.factory, "the Hyperliquid vault stack is already deployed");
            return Ok(vaults);
        }
        tracing::warn!(
            "{} names contracts that aren't there, or another deployment's factory or generation: deploying again",
            file.display()
        );
    }
    let vaults = deploy(client, artifacts, deployments, generation).await?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(file, serde_json::to_string_pretty(&vaults)? + "\n")?;
    // The deployment as the services would read it with this factory in
    // place, ready to be put where they read it once they run code that
    // knows the driver.
    // It carries the deployment's generation, as the chain's own file does.
    let adopted_file = file.with_extension("deployment.json");
    let adopted = adopted(deployments, &vaults);
    let adopted_text = match generation {
        Some(generation) => crate::deploy::deployment_json(&adopted, generation)?,
        None => serde_json::to_string_pretty(&adopted)?,
    };
    std::fs::write(&adopted_file, adopted_text + "\n")?;
    tracing::info!(file = %file.display(), deployment = %adopted_file.display(), "the Hyperliquid vault stack is written");
    Ok(vaults)
}

/// Deploy the stack. Its station factory is deployed afresh (not at an
/// address shared across chains: a Hyperliquid vault's station is on
/// HyperEVM alone), so no earlier stack's can be met; it is still read back
/// once initialised and refused, naming what differs, if it names anything
/// but this stack's vault factory, USDC, the registry and this stack's
/// station implementation.
async fn deploy(
    client: &Client,
    artifacts: &Path,
    deployments: &Deployments,
    generation: Option<u64>,
) -> Result<HyperliquidVaults> {
    let admin = deployments.roles.admin;
    let api = Api::for_chain(client.chain_id).ok_or_else(|| anyhow!("not HyperEVM"))?;
    let from_block = client.block_number().await?;

    // The venue's numbers for what the driver lists.
    let perp = perp_index(&api, PERP).await?;
    let spot_pair = api
        .spot_token(SPOT)
        .await?
        .usdc_pair
        .ok_or_else(|| anyhow!("{SPOT} has no pair against USDC"))?;
    let spot_pair = u32::try_from(spot_pair)?;
    tracing::info!(
        perp,
        spot_pair,
        "the venue's numbers for {PERP} and {SPOT}/USDC"
    );

    // The driver reads the venue's decimals from the precompiles in its
    // constructor, so its deploy is estimated against the chain itself.
    let driver = put(
        client,
        artifacts,
        admin,
        "HyperliquidDriver",
        (USDC, CORE_DEPOSIT_WALLET, vec![perp], vec![spot_pair]).abi_encode_params(),
    )
    .await?;
    client
        .send(
            admin,
            deployments.attestation,
            IAttestationRegistry::proposeDriverCall {
                name: crate::deploy::market_id("hyperliquid"),
                implementation: driver,
            },
        )
        .await
        .context("proposing the Hyperliquid driver")?;
    tracing::info!(%driver, "the Hyperliquid driver is proposed");

    // A generation whose own factory settles in USDC needs nothing more:
    // its factory and station factory are the stack's.
    let own_asset = client
        .read(deployments.factory, IFactoryParts::assetCall {})
        .await?;
    if own_asset == USDC {
        let station_implementation = client
            .read(
                deployments.station_factory,
                IStationFactoryParts::implementationCall {},
            )
            .await?;
        tracing::info!(factory = %deployments.factory, "the generation's own factory settles in USDC: it is the stack's");
        return Ok(HyperliquidVaults {
            chain_id: client.chain_id,
            deployed_block: from_block,
            usdc: USDC,
            core_deposit_wallet: CORE_DEPOSIT_WALLET,
            driver,
            station_implementation,
            station_factory: deployments.station_factory,
            factory: deployments.factory,
            previous_factory: deployments.factory,
            generation,
        });
    }

    // The station's closes run in a library linked into it.
    let closes = put(client, artifacts, admin, "StationCloses", Vec::new()).await?;
    let station_implementation = put_with(client, artifacts, admin, "Station", closes).await?;
    let station_factory = put(
        client,
        artifacts,
        admin,
        "StationFactory",
        (admin,).abi_encode_params(),
    )
    .await?;

    // Everything else as the deployment's factory has it.
    let previous = deployments.factory;
    let vault_implementation = client
        .read(previous, IFactoryParts::vaultImplementationCall {})
        .await?;
    let config = client.read(previous, IVaultFactory::configCall {}).await?;
    let chains = client.read(previous, IVaultFactory::chainsCall {}).await?;
    let factory = put(
        client,
        artifacts,
        admin,
        "VaultFactory",
        (
            (
                USDC,
                deployments.attestation,
                deployments.strategies,
                deployments.reviews,
                deployments.prices,
                station_factory,
                vault_implementation,
                admin,
                deployments.roles.platform,
                previous,
            ),
            config,
            chains,
        )
            .abi_encode_params(),
    )
    .await?;
    crate::deploy::initialise_station_factory(
        client,
        admin,
        station_factory,
        crate::deploy::StationFactorySet {
            attestation: deployments.attestation,
            asset: USDC,
            vault_factory: factory,
            implementation: station_implementation,
        },
    )
    .await?;

    Ok(HyperliquidVaults {
        chain_id: client.chain_id,
        deployed_block: from_block,
        usdc: USDC,
        core_deposit_wallet: CORE_DEPOSIT_WALLET,
        driver,
        station_implementation,
        station_factory,
        factory,
        previous_factory: previous,
        generation,
    })
}

/// A perp's index, by its coin, from the venue's list.
async fn perp_index(api: &Api, coin: &str) -> Result<u32> {
    let meta = api.info(json!({"type": "meta"})).await?;
    let universe = meta["universe"]
        .as_array()
        .ok_or_else(|| anyhow!("the venue's meta has no universe"))?;
    let index = universe
        .iter()
        .position(|asset| asset["name"] == coin)
        .ok_or_else(|| anyhow!("the venue lists no perp {coin}"))?;
    Ok(u32::try_from(index)?)
}

/// Deploy one contract from the bytecode the build tool left behind.
async fn put(
    client: &Client,
    artifacts: &Path,
    from: Address,
    name: &str,
    arguments: Vec<u8>,
) -> Result<Address> {
    put_linking(client, artifacts, from, name, arguments, None).await
}

/// A contract that links one library, at `library`.
async fn put_with(
    client: &Client,
    artifacts: &Path,
    from: Address,
    name: &str,
    library: Address,
) -> Result<Address> {
    put_linking(client, artifacts, from, name, Vec::new(), Some(library)).await
}

async fn put_linking(
    client: &Client,
    artifacts: &Path,
    from: Address,
    name: &str,
    arguments: Vec<u8>,
    library: Option<Address>,
) -> Result<Address> {
    let path = artifacts
        .join(format!("{name}.sol"))
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "reading {}. Run the contracts' build first: `task contracts:build`",
            path.display()
        )
    })?;
    let artifact: serde_json::Value = serde_json::from_str(&text)?;
    let mut hex = artifact["bytecode"]["object"]
        .as_str()
        .ok_or_else(|| anyhow!("{name} has no bytecode"))?
        .to_string();
    // Each place the library is called holds a 40-character placeholder.
    if let Some(library) = library {
        while let Some(start) = hex.find("__$") {
            hex.replace_range(start..start + 40, &alloy::hex::encode(library));
        }
    }
    let mut bytecode = alloy::hex::decode(hex.trim_start_matches("0x"))
        .with_context(|| format!("{name}'s bytecode (it links no libraries)"))?;
    bytecode.extend_from_slice(&arguments);
    // A fixed gas limit on HyperEVM rather than an endpoint's estimate.
    let address = client
        .deploy_with_gas_limit(from, bytecode, crate::deploy::creation_gas(client))
        .await?;
    tracing::info!(%address, contract = name, "deployed");
    Ok(address)
}

// A run's own steps ---------------------------------------------------------

/// The funder, as an account on HyperCore and on HyperEVM.
pub struct Funder {
    core: HyperCore,
    api: Api,
    signer: PrivateKeySigner,
}

impl Funder {
    pub fn new(client: &Client) -> Result<Self> {
        let signer = crate::fund::funder()?;
        let api = Api::for_chain(client.chain_id).ok_or_else(|| anyhow!("not HyperEVM"))?;
        Ok(Self {
            core: HyperCore::new(api.clone(), signer.clone()),
            api,
            signer,
        })
    }

    /// Make sure `to` holds `dollars` of USDC on HyperEVM: from the funder's
    /// HyperCore spot balance home to its own HyperEVM address, then on.
    /// The perp side tops the spot side up first if it is short.
    pub async fn dollars_on_hyperevm(
        &self,
        client: &Client,
        to: Address,
        dollars: f64,
    ) -> Result<()> {
        let me = self.signer.address();
        // Through the client's own connection: an endpoint's URL may carry
        // a key, and an error must never name it.
        let provider = client.signing_provider(self.signer.clone())?;
        let wanted = U256::from((dollars * 1e6).round() as u64);
        // Someone who already holds it is given nothing more.
        if client
            .read(USDC, IERC20::balanceOfCall { account: to })
            .await?
            >= wanted
        {
            return Ok(());
        }
        let held = || client.read(USDC, IERC20::balanceOfCall { account: me });
        let before = held().await?;
        if before < wanted {
            let usdc = self.api.spot_token("USDC").await?;
            let spot = self.api.spot_balance(me, "USDC").await?;
            let short = dollars - spot;
            if short > 0.0 {
                self.core
                    .usd_class_transfer((short + 1.0).ceil(), false)
                    .await
                    .context("moving the funder's perp dollars to spot")?;
            }
            self.core
                .spot_send(USDC_SYSTEM, &usdc, dollars)
                .await
                .context("sending the funder's USDC home to HyperEVM")?;
            let mut landed = false;
            for _ in 0..60 {
                if held().await? >= before + wanted {
                    landed = true;
                    break;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            if !landed {
                bail!("the funder's USDC never reached HyperEVM");
            }
        }
        let receipt = provider
            .send_transaction(
                alloy::rpc::types::TransactionRequest::default()
                    .to(USDC)
                    .input(
                        IERC20::transferCall { to, value: wanted }
                            .abi_encode()
                            .into(),
                    ),
            )
            .await?
            .get_receipt()
            .await?;
        if !receipt.status() {
            bail!("the USDC transfer to {to} reverted");
        }
        tracing::info!(%to, dollars, tx = %receipt.transaction_hash, "USDC on HyperEVM");
        Ok(())
    }

    /// Fund a station's HyperCore account directly, as far as it isn't
    /// already: `dollars` to its perp balance (which brings the account into
    /// being, for the venue's $1) unless its account is already worth that,
    /// and HYPE on its spot side up to `hype`, for the sends home its closes
    /// make. The funder buys the HYPE first if it holds too little. Safe to
    /// run again: what is there is not sent twice.
    pub async fn station_account(&self, station: Address, dollars: f64, hype: f64) -> Result<()> {
        // A read that fails stops here: taken for an empty account, the
        // station would be sent its dollars and HYPE again.
        let worth = account_value(&self.api, station).await?;
        if worth < dollars * 0.9 {
            // A usd send pays from the funder's perp side; the faucet pays
            // to its spot side, so move what the perp side lacks first.
            let me = self.signer.address();
            let perp = account_value(&self.api, me).await?;
            if perp < dollars {
                self.core
                    .usd_class_transfer((dollars - perp + 1.0).ceil(), true)
                    .await
                    .context("moving the funder's spot dollars to its perp side")?;
            }
            self.core
                .usd_send(station, dollars)
                .await
                .context("sending the station's account its dollars")?;
            tracing::info!(%station, millionths = (dollars * 1e6).round() as u64, "the station's HyperCore account was sent dollars");
        } else {
            tracing::info!(%station, worth, "the station's HyperCore account already holds its dollars");
        }
        let held = spot_total(&self.api, station, SPOT).await?;
        let short = hype - held;
        if short > 0.0 {
            let token = self.api.spot_token(SPOT).await?;
            let send = round_up(short, 8);
            self.hype_on_hand(&token, send).await?;
            self.core
                .spot_send(station, &token, send)
                .await
                .context("sending the station's account HYPE for its sends")?;
            tracing::info!(%station, hype = send, "the station's HyperCore account was sent HYPE");
        }
        Ok(())
    }

    /// Make sure the funder holds `amount` HYPE on HyperCore's spot side,
    /// buying what it lacks (at least the venue's $10 minimum, at no more
    /// than [`HYPE_CEILING`] a HYPE).
    async fn hype_on_hand(&self, token: &hyperliquid::SpotToken, amount: f64) -> Result<()> {
        let me = self.signer.address();
        let held = self.api.spot_balance(me, SPOT).await?;
        if held >= amount {
            return Ok(());
        }
        let pair = token
            .usdc_pair
            .ok_or_else(|| anyhow!("{SPOT} has no pair against USDC"))?;
        let (_, ask) = self.api.best_bid_ask(&format!("@{pair}")).await?;
        let ask = ask.ok_or_else(|| anyhow!("nobody is selling {SPOT} on HyperCore"))?;
        if ask > HYPE_CEILING {
            bail!("{SPOT}'s best ask is ${ask}, over the ${HYPE_CEILING} this run pays");
        }
        let size = round_up((amount - held).max(12.0 / ask), token.sz_decimals);
        let limit = (ask * 1.03).min(HYPE_CEILING);
        let limit = (limit * 100.0).ceil() / 100.0;
        let cost = size * limit * 1.01;
        let spot_usdc = self.api.spot_balance(me, "USDC").await?;
        if spot_usdc < cost {
            self.core
                .usd_class_transfer((cost - spot_usdc).ceil() + 1.0, false)
                .await
                .context("moving the funder's perp dollars to spot")?;
        }
        let fill = self.core.spot_ioc(pair, true, size, limit).await?;
        tracing::info!(
            size = fill.size,
            price = fill.average_price,
            "the funder bought {SPOT}"
        );
        Ok(())
    }
}

/// How long a run follows its claim: a close is staged, the venue acts, and
/// it is delivered within the driver's five-minute window, perhaps in parts,
/// each staged after a valuation.
const CLAIM_MINUTES: u64 = 60;

/// The most the funder pays for a HYPE when it buys some for a station.
const HYPE_CEILING: f64 = 100.0;
/// The HYPE a station's account is given for its sends home: above the
/// executor's own floor, so it doesn't start by buying.
pub const STATION_HYPE: f64 = 0.06;

fn round_up(value: f64, decimals: u32) -> f64 {
    let scale = 10f64.powi(decimals as i32);
    (value * scale).ceil() / scale
}

/// An account's perp side at mark, in dollars.
async fn account_value(api: &Api, user: Address) -> Result<f64> {
    let state = api
        .info(json!({"type": "clearinghouseState", "user": user}))
        .await?;
    state["marginSummary"]["accountValue"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| anyhow!("HyperCore's account value for {user} can't be read: {state}"))
}

/// What `user` holds of spot token `token` on HyperCore: zero if the answer
/// lists none, an error if the answer can't be read.
async fn spot_total(api: &Api, user: Address, token: &str) -> Result<f64> {
    let state = api
        .info(json!({"type": "spotClearinghouseState", "user": user}))
        .await?;
    let balances = state["balances"]
        .as_array()
        .ok_or_else(|| anyhow!("HyperCore's spot balances for {user} can't be read: {state}"))?;
    match balances.iter().find(|balance| balance["coin"] == token) {
        None => Ok(0.0),
        Some(balance) => balance["total"]
            .as_str()
            .and_then(|total| total.parse().ok())
            .ok_or_else(|| {
                anyhow!("HyperCore's {token} balance for {user} can't be read: {balance}")
            }),
    }
}

/// Activate a published, reviewed strategy at the Hyperliquid factory: the
/// tank in USDC, the modest allowances, the author's statement signed. A
/// vault already activated for it is returned as it is.
pub async fn activate(
    client: &Client,
    vaults: &HyperliquidVaults,
    author: Address,
    strategy_id: u64,
) -> Result<Address> {
    let id = U256::from(strategy_id);
    let factory = vaults.factory;
    let vault = client
        .read(factory, IVaultFactory::predictVaultCall { strategyId: id })
        .await?;
    if client.has_code(vault).await? {
        tracing::info!(%vault, "already activated");
        return Ok(vault);
    }
    let tank = client
        .read(factory, IVaultFactory::configCall {})
        .await?
        .minimumTank;
    client
        .send(
            author,
            vaults.usdc,
            IERC20::approveCall {
                spender: factory,
                value: tank,
            },
        )
        .await
        .context("approving the tank")?;
    let restricted = client
        .read(factory, IVaultFactory::restrictedForCall { strategyId: id })
        .await?
        .text;
    let statement = client.sign_statement(author, vault, &restricted).await?;
    let terms = client.read(factory, IVaultFactory::termsCall {}).await?;
    let receipt = client
        .send(
            author,
            factory,
            IVaultFactory::activateCall {
                strategyId: id,
                allowances: chain::allowances(&Allowances::modest()),
                idleFloorBps: 500,
                tankFunding: tank,
                tankKeep: U256::ZERO,
                statement,
                terms,
            },
        )
        .await
        .context("activating the vault")?;
    tracing::info!(%vault, tank_units = %tank, tx = %receipt.transaction_hash, "activated");
    Ok(vault)
}

/// Deposit `amount` millionths of USDC into `vault` from `who`.
pub async fn deposit(
    client: &Client,
    vaults: &HyperliquidVaults,
    vault: Address,
    who: Address,
    amount: u64,
) -> Result<()> {
    let amount = U256::from(amount);
    client
        .send(
            who,
            vaults.usdc,
            IERC20::approveCall {
                spender: vault,
                value: amount,
            },
        )
        .await?;
    let restricted = client.read(vault, IVault::restrictedTextCall {}).await?;
    let statement = client.sign_statement(who, vault, &restricted).await?;
    let receipt = client
        .send(
            who,
            vault,
            IVault::requestDepositCall {
                assets: amount,
                receiver: who,
                statement,
            },
        )
        .await
        .context("the deposit")?;
    tracing::info!(%vault, %who, units = %amount, tx = %receipt.transaction_hash, "deposit requested");
    Ok(())
}

/// Wait for `who`'s shares, for up to `minutes`: a deposit is priced at a
/// valuation the keeper sends.
pub async fn shares_of(
    client: &Client,
    vault: Address,
    who: Address,
    minutes: u64,
) -> Result<U256> {
    for _ in 0..minutes * 12 {
        let held = client
            .read(vault, IVault::balanceOfCall { owner: who })
            .await?;
        if !held.is_zero() {
            return Ok(held);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    bail!("the deposit was never priced: is the keeper keeping the Hyperliquid factory?")
}

/// The vault's latest valuation, read back and logged in base units.
pub async fn log_valuation(client: &Client, vault: Address) -> Result<u64> {
    let count = client
        .read(vault, IVault::valuationCountCall {})
        .await?
        .to::<u64>();
    if count == 0 {
        bail!("the vault has no valuation yet");
    }
    let valuation = client
        .read(
            vault,
            IVault::valuationAtCall {
                index: U256::from(count - 1),
            },
        )
        .await?;
    tracing::info!(
        %vault,
        index = count - 1,
        time = valuation.time,
        status = valuation.status as u8,
        gross_units = %valuation.gross,
        gross_exit_units = %valuation.grossExit,
        supply_units = %valuation.supply,
        cash_units = %valuation.cash,
        "a valuation"
    );
    Ok(count)
}

/// The station's fills on HyperCore since `since_ms`, as the venue records
/// them, each logged once (by its trade id) as the venue writes its price
/// and size.
async fn log_fills(api: &Api, station: Address, since_ms: u64, seen: &mut Vec<u64>) -> Result<()> {
    let fills = api
        .info(json!({"type": "userFillsByTime", "user": station, "startTime": since_ms}))
        .await?;
    for fill in fills.as_array().into_iter().flatten() {
        let id = fill["tid"].as_u64().unwrap_or_default();
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        tracing::info!(
            %station,
            coin = %fill["coin"],
            side = %fill["side"],
            price = %fill["px"],
            size = %fill["sz"],
            fee = %fill["fee"],
            order = %fill["oid"],
            "a fill on HyperCore, through the executor's key"
        );
    }
    Ok(())
}

/// Ask for all of `who`'s shares back, unless a claim of theirs is already
/// booked, and follow that claim until it is paid: staged at the station,
/// delivered from HyperCore, and paid from its own delivery. Each change of
/// state is logged with what was paid, in base units. Fails loudly if the
/// withdrawal is paid from cash instead of booking a claim.
pub async fn claim_all(
    client: &Client,
    vault: Address,
    who: Address,
    known: Option<u64>,
    minutes: u64,
    noted: impl Fn(u64) -> Result<()>,
) -> Result<U256> {
    // A claim booked by an earlier run that stopped before it noted the
    // claim is followed, not asked for again.
    let known = match known {
        Some(id) => Some(id),
        None => {
            let found = newest_claim(client, vault, who, 0, false).await?;
            if let Some(id) = found {
                tracing::info!(%vault, claim = id, "a claim is already booked");
                noted(id)?;
            }
            found
        }
    };
    let claim_id = match known {
        Some(id) => id,
        None => {
            let shares = client
                .read(vault, IVault::balanceOfCall { owner: who })
                .await?;
            if shares.is_zero() {
                bail!("{who} holds no shares to ask back");
            }
            let claims_before = client
                .read(vault, IVault::claimCountCall {})
                .await?
                .to::<u64>();
            let receipt = client
                .send(
                    who,
                    vault,
                    IVault::requestRedeemCall {
                        shares,
                        receiver: who,
                    },
                )
                .await
                .context("the withdrawal")?;
            let request = client.read(vault, IVault::requestCountCall {}).await?;
            tracing::info!(%vault, shares_units = %shares, request = %request, tx = %receipt.transaction_hash, "withdrawal requested");
            // Booked at the next valuation, if the cash can't pay it. The
            // request can be read as done a moment before the claim it
            // booked (two reads either side of the valuation, or on two
            // endpoints a block apart), so it is taken as paid from cash
            // only once the claim still isn't there a little later.
            let mut booked = None;
            let mut done_since: Option<tokio::time::Instant> = None;
            for _ in 0..minutes * 12 {
                booked = newest_claim(client, vault, who, claims_before, true).await?;
                if booked.is_some() {
                    break;
                }
                if client
                    .read(vault, IVault::requestAtCall { requestId: request })
                    .await?
                    .done
                {
                    let since = *done_since.get_or_insert_with(tokio::time::Instant::now);
                    if since.elapsed() >= CLAIM_SEEN_WITHIN {
                        bail!("the withdrawal was paid from the vault's cash: no claim was booked");
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            let id = booked.ok_or_else(|| anyhow!("no claim was booked in {minutes} minutes"))?;
            tracing::info!(%vault, claim = id, "a claim is booked");
            noted(id)?;
            id
        }
    };
    // Each change is logged: a state (`Waiting`, `Closing` while the close
    // staged for it runs, `Paid`), and each part paid, which sends a claim
    // back to `Waiting` with fewer shares and more paid.
    let mut last = None;
    for _ in 0..minutes * 12 {
        let claim = client
            .read(
                vault,
                IVault::claimAtCall {
                    claimId: U256::from(claim_id),
                },
            )
            .await?;
        let seen = (claim.state as u8, claim.shares, claim.paid);
        if last != Some(seen) {
            let part_paid = last.is_some_and(|(_, _, paid)| claim.paid > paid)
                && !matches!(claim.state, ClaimState::Paid);
            tracing::info!(
                %vault,
                claim = claim_id,
                state = claim_state_name(claim.state as u8),
                shares_units = %claim.shares,
                paid_units = %claim.paid,
                "{}",
                if part_paid {
                    "a part of the claim is paid"
                } else {
                    "the claim"
                }
            );
            last = Some(seen);
        }
        if matches!(claim.state, ClaimState::Paid) {
            let held = client
                .read(USDC, IERC20::balanceOfCall { account: who })
                .await?;
            tracing::info!(%vault, claim = claim_id, paid_units = %claim.paid, depositor_usdc_units = %held, "the claim is paid");
            return Ok(claim.paid);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    bail!("claim {claim_id} was not paid in {minutes} minutes")
}

/// How long after a withdrawal is read as done its claim is still looked
/// for, before the withdrawal is taken as paid from cash.
const CLAIM_SEEN_WITHIN: Duration = Duration::from_secs(30);

/// The newest claim of `who`'s numbered above `after`: any of them, or with
/// `settled` false only one still waiting to be paid.
async fn newest_claim(
    client: &Client,
    vault: Address,
    who: Address,
    after: u64,
    settled: bool,
) -> Result<Option<u64>> {
    let count = client
        .read(vault, IVault::claimCountCall {})
        .await?
        .to::<u64>();
    let mut found = None;
    for id in after + 1..=count {
        let claim = client
            .read(
                vault,
                IVault::claimAtCall {
                    claimId: U256::from(id),
                },
            )
            .await?;
        let waiting = !matches!(claim.state, ClaimState::Paid | ClaimState::Fallback);
        if claim.owner == who && (settled || waiting) {
            found = Some(id);
        }
    }
    Ok(found)
}

/// A claim state's name, by its number in the vault's `ClaimState`.
fn claim_state_name(state: u8) -> &'static str {
    const NAMES: [&str; 7] = [
        "None", "Pending", "Waiting", "Queued", "Paid", "Fallback", "Closing",
    ];
    NAMES.get(usize::from(state)).copied().unwrap_or("unknown")
}

/// How far a run has got, kept beside the deployment so a run stopped
/// part-way picks up where it stopped rather than spending its testnet
/// dollars again (on testnet, money sent toward HyperCore from HyperEVM can't
/// always be got back).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub agent: String,
    pub strategy_id: Option<u64>,
    pub vault: Option<Address>,
    /// The deposit, in USDC millionths, once it was sent.
    pub deposited: Option<u64>,
    pub priced: bool,
    pub station_funded: bool,
    /// A second depositor's deposit is priced.
    #[serde(default)]
    pub stayed: bool,
    pub claim: Option<u64>,
    pub paid: Option<String>,
}

impl Progress {
    pub fn file_in(deployments_dir: &Path, agent: &str) -> PathBuf {
        deployments_dir.join(format!("hyperliquid-{CHAIN_ID}.run-{agent}.json"))
    }

    fn read(path: &Path, agent: &str) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(serde_json::from_str(&text)
                .with_context(|| format!("reading {}", path.display()))?),
            Err(_) => Ok(Self {
                agent: agent.to_string(),
                ..Self::default()
            }),
        }
    }

    fn write(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)? + "\n")
            .with_context(|| format!("writing {}", path.display()))
    }
}

/// What a run is given.
pub struct RunOptions<'a> {
    pub api: &'a str,
    pub agent: &'a Path,
    /// The depositor's deposit, in dollars: at least the factory's minimum.
    pub deposit_dollars: Option<f64>,
    /// What the funder sends the station's HyperCore account.
    pub station_dollars: f64,
    /// How long the agent trades before the claim, at least.
    pub trade_minutes: u64,
}

/// The whole run on a testnet whose services read [`adopted`]: deploy (or
/// find) the stack; publish the agent through the API; activate it at the
/// Hyperliquid factory; the depositor's USDC in and priced; the station's
/// HyperCore account funded so the agent has a venue balance to trade; the
/// agent's fills through the executor read back from the venue; a valuation
/// read back; then all of the depositor's shares asked back and the claim
/// followed until it is paid. Each step is read back and logged in base
/// units, and each is recorded in [`Progress`] as it is done, so a run
/// stopped part-way can be started again with the same arguments.
///
/// What an agent can do depends on the vault's size: `mm-quoter` quotes 2%
/// of the vault's value a side and posts nothing under $12, so it needs a
/// vault of about $650 (`--deposit 650`).
pub async fn run(
    client: &Client,
    artifacts: &Path,
    deployments: &Deployments,
    generation: Option<u64>,
    deployments_dir: &Path,
    options: RunOptions<'_>,
) -> Result<()> {
    let file = HyperliquidVaults::file_in(deployments_dir);
    let vaults = ensure(client, artifacts, deployments, generation, &file).await?;
    if deployments.factory != vaults.factory {
        bail!(
            "the services don't read the Hyperliquid factory yet, so nothing would price this vault's deposit or serve its claim: put {} in place of the chain's deployment file (keep the original), start the services again, and run this again",
            file.with_extension("deployment.json").display()
        );
    }
    let agent_name = options
        .agent
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("agent")
        .to_string();
    let progress_file = Progress::file_in(deployments_dir, &agent_name);
    let mut progress = Progress::read(&progress_file, &agent_name)?;
    let author = client.account(Role::Author)?;
    let depositor = client.account(Role::Viewer)?;
    let funder = Funder::new(client)?;
    let api = Api::for_chain(client.chain_id).ok_or_else(|| anyhow!("not HyperEVM"))?;

    // A generation in USDC charges its publishing fee in USDC too: the
    // author is given the fee and the tank before it publishes.
    if progress.strategy_id.is_none() && deployments.usd == USDC {
        let fee = client
            .read(
                deployments.publishing,
                chain::bindings::publishing::IPublishing::feeCall {},
            )
            .await?;
        let tank = client
            .read(vaults.factory, IVaultFactory::configCall {})
            .await?
            .minimumTank;
        let dollars = u64::try_from(fee + tank).unwrap_or(u64::MAX) as f64 / 1e6;
        funder.dollars_on_hyperevm(client, author, dollars).await?;
    }
    let strategy_id = match progress.strategy_id {
        Some(id) => id,
        None => {
            let published = crate::publish::publish(
                client,
                shared::Chain::Hyperliquid,
                deployments,
                options.api,
                options.agent,
                author,
                false,
                false,
            )
            .await?;
            let id = published
                .strategy_id
                .ok_or_else(|| anyhow!("the agent was not registered: {:?}", published.outcome))?;
            tracing::info!(strategy_id = id, outcome = ?published.outcome, "published");
            progress.strategy_id = Some(id);
            progress.write(&progress_file)?;
            id
        }
    };

    let config = client
        .read(vaults.factory, IVaultFactory::configCall {})
        .await?;
    let tank_dollars = config.minimumTank.to::<u64>() as f64 / 1e6;
    let least = config.minimumDeposit.to::<u64>().max(20_000_000);
    let deposit_units = match options.deposit_dollars {
        Some(dollars) => {
            let units = (dollars * 1e6).round() as u64;
            if units < config.minimumDeposit.to::<u64>() {
                bail!("a deposit of ${dollars} is under the factory's minimum");
            }
            units
        }
        None => least,
    };

    let vault = match progress.vault {
        Some(vault) => vault,
        None => {
            funder
                .dollars_on_hyperevm(client, author, tank_dollars)
                .await?;
            // The review is posted by the reviewer service; activation waits
            // for it.
            for attempt in 0.. {
                if client
                    .read(
                        deployments.reviews,
                        chain::bindings::reviews::IReviewRegistry::hasReviewCall {
                            strategyId: U256::from(strategy_id),
                        },
                    )
                    .await?
                {
                    break;
                }
                if attempt == 120 {
                    bail!("the review of {strategy_id} was never posted");
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            let vault = activate(client, &vaults, author, strategy_id).await?;
            progress.vault = Some(vault);
            progress.write(&progress_file)?;
            vault
        }
    };

    if progress.deposited.is_none() {
        funder
            .dollars_on_hyperevm(client, depositor, deposit_units as f64 / 1e6)
            .await?;
        deposit(client, &vaults, vault, depositor, deposit_units).await?;
        progress.deposited = Some(deposit_units);
        progress.write(&progress_file)?;
    }
    if !progress.priced {
        let shares = shares_of(client, vault, depositor, 15).await?;
        tracing::info!(%vault, deposit_units = progress.deposited.unwrap_or(deposit_units), shares_units = %shares, "the deposit is priced");
        log_valuation(client, vault).await?;
        progress.priced = true;
        progress.write(&progress_file)?;
    }

    // Funded once the depositor holds the shares, so what the account
    // brings is theirs rather than the dead shares'. The executor's agent
    // approval is queued at the venue and dropped while the account doesn't
    // exist; it approves again within a minute once it reads it missing.
    let station = client.read(vault, IVault::homeStationCall {}).await?;
    if !progress.station_funded {
        funder
            .station_account(station, options.station_dollars, STATION_HYPE)
            .await?;
        progress.station_funded = true;
        progress.write(&progress_file)?;
    }

    if progress.claim.is_none() {
        // The agent trades; its fills are read back from the venue as they
        // come.
        let since_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64
            - options.trade_minutes * 60_000
            - 60_000;
        let mut seen = Vec::new();
        let until = tokio::time::Instant::now() + Duration::from_secs(options.trade_minutes * 60);
        while tokio::time::Instant::now() < until {
            if let Err(error) = log_fills(&api, station, since_ms, &mut seen).await {
                tracing::warn!(error = %format!("{error:#}"), "the venue's fills couldn't be read");
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        log_fills(&api, station, since_ms, &mut seen).await?;
        if seen.is_empty() {
            tracing::warn!(%station, "no fill through the executor yet: the agent placed nothing that traded");
        }
        let worth = account_value(&api, station).await.unwrap_or(0.0);
        tracing::info!(%station, account_value_millionths = (worth * 1e6).round() as u64, "the station's HyperCore account");
        log_valuation(client, vault).await?;
    }

    // A second depositor, who stays, so the leaver isn't nearly the whole
    // vault. The driver stages no close for a fraction so near all of the
    // station that what would be left couldn't cover the close's worst
    // slippage, and while the agent holds a position such a leaver's claim
    // would wait for it to be flat. They put in the least a deposit may be:
    // the leaver's claim reaches the venue only while the stayer's cash is
    // less than what the station holds there, or the cash pays it all.
    if !progress.stayed {
        let units = least;
        if units as f64 / 1e6 >= options.station_dollars {
            tracing::warn!(
                stays_units = units,
                station_dollars = options.station_dollars,
                "the second depositor's cash covers what the station holds at the venue: the leaver will be paid from cash, with no close staged"
            );
        }
        funder
            .dollars_on_hyperevm(client, author, units as f64 / 1e6)
            .await?;
        deposit(client, &vaults, vault, author, units).await?;
        let shares = shares_of(client, vault, author, 15).await?;
        tracing::info!(%vault, deposit_units = units, shares_units = %shares, "a second depositor's deposit is priced, and they stay");
        progress.stayed = true;
        progress.write(&progress_file)?;
    }

    let paid = claim_all(
        client,
        vault,
        depositor,
        progress.claim,
        CLAIM_MINUTES,
        |id| {
            let mut noted = progress.clone();
            noted.claim = Some(id);
            noted.write(&progress_file)
        },
    )
    .await?;
    progress = Progress::read(&progress_file, &agent_name)?;
    progress.paid = Some(paid.to_string());
    progress.write(&progress_file)?;
    log_valuation(client, vault).await?;
    tracing::info!(%vault, "the run is done: a valuation, a deposit priced and a claim paid (any fills are logged above)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stack_s_file_reads_back_as_written() {
        let vaults = HyperliquidVaults {
            chain_id: CHAIN_ID,
            deployed_block: 7,
            usdc: USDC,
            core_deposit_wallet: CORE_DEPOSIT_WALLET,
            driver: Address::repeat_byte(1),
            station_implementation: Address::repeat_byte(2),
            station_factory: Address::repeat_byte(3),
            factory: Address::repeat_byte(4),
            previous_factory: Address::repeat_byte(5),
            generation: Some(2),
        };
        let text = serde_json::to_string(&vaults).unwrap();
        assert_eq!(
            serde_json::from_str::<HyperliquidVaults>(&text).unwrap(),
            vaults
        );
        assert!(HyperliquidVaults::file_in(Path::new("/x")).ends_with("hyperliquid-998.json"));
    }

    #[test]
    fn a_run_s_progress_reads_back_as_written_and_starts_empty() {
        let dir = std::env::temp_dir().join(format!("shonin-hl-progress-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = Progress::file_in(&dir, "mm-quoter");
        assert!(file.ends_with("hyperliquid-998.run-mm-quoter.json"));
        let fresh = Progress::read(&file, "mm-quoter").unwrap();
        assert_eq!(fresh.agent, "mm-quoter");
        assert_eq!(fresh.vault, None);
        let progress = Progress {
            agent: "mm-quoter".to_string(),
            strategy_id: Some(3),
            vault: Some(Address::repeat_byte(7)),
            deposited: Some(650_000_000),
            priced: true,
            station_funded: true,
            stayed: true,
            claim: Some(1),
            paid: None,
        };
        progress.write(&file).unwrap();
        assert_eq!(Progress::read(&file, "mm-quoter").unwrap(), progress);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_adopted_deployment_swaps_the_factories_and_nothing_else() {
        let deployments = Deployments::from_json(
            &json!({
                "chain_key": "evm:998", "chain_id": 998, "rpc_url": "http://x",
                "deployed_block": 1,
                "usd": format!("{:?}", Address::repeat_byte(0xaa)),
                "venue": format!("{:?}", Address::ZERO),
                "attestation_verifier": format!("{:?}", Address::ZERO),
                "attestation": format!("{:?}", Address::ZERO),
                "publishing": format!("{:?}", Address::ZERO),
                "prices": format!("{:?}", Address::ZERO),
                "strategies": format!("{:?}", Address::ZERO),
                "reviews": format!("{:?}", Address::ZERO),
                "profiles": format!("{:?}", Address::ZERO),
                "factory": format!("{:?}", Address::repeat_byte(5)),
                "station_factory": format!("{:?}", Address::repeat_byte(6)),
                "driver": format!("{:?}", Address::ZERO),
                "bridge": format!("{:?}", Address::ZERO),
                "roles": {
                    "admin": format!("{:?}", Address::ZERO),
                    "platform": format!("{:?}", Address::ZERO),
                    "attestor": format!("{:?}", Address::ZERO),
                    "builder": format!("{:?}", Address::ZERO),
                    "reviewer": format!("{:?}", Address::ZERO),
                    "executor": format!("{:?}", Address::ZERO),
                    "keeper": format!("{:?}", Address::ZERO),
                    "operator": format!("{:?}", Address::ZERO),
                    "author": format!("{:?}", Address::ZERO),
                    "viewer": format!("{:?}", Address::ZERO),
                }
            })
            .to_string(),
        )
        .unwrap();
        let vaults = HyperliquidVaults {
            chain_id: CHAIN_ID,
            deployed_block: 7,
            usdc: USDC,
            core_deposit_wallet: CORE_DEPOSIT_WALLET,
            driver: Address::repeat_byte(1),
            station_implementation: Address::repeat_byte(2),
            station_factory: Address::repeat_byte(3),
            factory: Address::repeat_byte(4),
            previous_factory: Address::repeat_byte(5),
            generation: Some(2),
        };
        let adopted = adopted(&deployments, &vaults);
        assert_eq!(adopted.factory, vaults.factory);
        assert_eq!(adopted.station_factory, vaults.station_factory);
        // The publishing fee is still paid in the deployment's own dollar.
        assert_eq!(adopted.usd, deployments.usd);
        assert_eq!(adopted.deployed_block, deployments.deployed_block);
    }
}
