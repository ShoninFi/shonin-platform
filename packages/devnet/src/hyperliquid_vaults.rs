//! Hyperliquid vaults on Hyperliquid testnet, whose dollar is HyperEVM's own
//! USDC rather than the stand-in dollar.
//!
//! The testnet deploy puts the stand-in stack on HyperEVM like on every
//! chain: a vault there settles in the stand-in dollar, which the venue has
//! never heard of. A vault that trades on Hyperliquid itself needs a factory
//! whose asset is the USDC that HyperCore sends home, a station factory with
//! the same asset, the station built with delayed closes, and the Hyperliquid
//! driver approved. This deploys those beside the testnet deployment, using
//! its registries, prices and vault implementation:
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

use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol_types::{SolCall, SolValue};
use anyhow::{Context, Result, anyhow, bail};
use chain::bindings::attestation::IAttestationRegistry;
use chain::bindings::erc20::IERC20;
use chain::bindings::factory::IVaultFactory;
use chain::bindings::station::IStationFactory;
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
        let mut all_there = vaults.previous_factory == deployments.factory;
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
            "{} names contracts that aren't there, or another deployment's factory: deploying again",
            file.display()
        );
    }
    let vaults = deploy(client, artifacts, deployments).await?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(file, serde_json::to_string_pretty(&vaults)? + "\n")?;
    // The deployment as the services would read it with this factory in
    // place, ready to be put where they read it once they run code that
    // knows the driver.
    let adopted_file = file.with_extension("deployment.json");
    std::fs::write(
        &adopted_file,
        serde_json::to_string_pretty(&adopted(deployments, &vaults))? + "\n",
    )?;
    tracing::info!(file = %file.display(), deployment = %adopted_file.display(), "the Hyperliquid vault stack is written");
    Ok(vaults)
}

async fn deploy(
    client: &Client,
    artifacts: &Path,
    deployments: &Deployments,
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
    let station_implementation = put(client, artifacts, admin, "Station", Vec::new()).await?;
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
    client
        .send(
            admin,
            station_factory,
            IStationFactory::initializeCall {
                attestation_: deployments.attestation,
                asset_: USDC,
                vaultFactory_: factory,
                implementation_: station_implementation,
            },
        )
        .await
        .context("initialising the station factory")?;
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
    let hex = artifact["bytecode"]["object"]
        .as_str()
        .ok_or_else(|| anyhow!("{name} has no bytecode"))?;
    let mut bytecode = alloy::hex::decode(hex.trim_start_matches("0x"))
        .with_context(|| format!("{name}'s bytecode (it links no libraries)"))?;
    bytecode.extend_from_slice(&arguments);
    let address = client.deploy(from, bytecode).await?;
    tracing::info!(%address, contract = name, "deployed");
    Ok(address)
}

// A run's own steps ---------------------------------------------------------

/// The funder, as an account on HyperCore and on HyperEVM.
pub struct Funder {
    core: HyperCore,
    api: Api,
    signer: PrivateKeySigner,
    rpc: String,
}

impl Funder {
    pub fn new(client: &Client) -> Result<Self> {
        let signer = crate::fund::funder()?;
        let api = Api::for_chain(client.chain_id).ok_or_else(|| anyhow!("not HyperEVM"))?;
        Ok(Self {
            core: HyperCore::new(api.clone(), signer.clone()),
            api,
            signer,
            // A chain may name several endpoints, one after another.
            rpc: client
                .rpc_url
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string(),
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
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(self.signer.clone()))
            .connect_http(self.rpc.parse()?);
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

    /// Fund a station's HyperCore account directly: `dollars` to its perp
    /// balance (which brings the account into being, for the venue's $1),
    /// and a little HYPE on its spot side for the sends home its closes
    /// make.
    pub async fn station_account(&self, station: Address, dollars: f64, hype: f64) -> Result<()> {
        self.core
            .usd_send(station, dollars)
            .await
            .context("sending the station's account its dollars")?;
        let token = self.api.spot_token(SPOT).await?;
        self.core
            .spot_send(station, &token, hype)
            .await
            .context("sending the station's account HYPE for its sends")?;
        tracing::info!(%station, dollars, hype, "the station's HyperCore account is funded");
        Ok(())
    }
}

/// Activate a published, reviewed strategy at the Hyperliquid factory: the
/// tank in USDC, the modest allowances, the author's statement signed.
pub async fn activate(
    client: &Client,
    vaults: &HyperliquidVaults,
    author: Address,
    strategy_id: u64,
) -> Result<Address> {
    let id = U256::from(strategy_id);
    let factory = vaults.factory;
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
    let vault = client
        .read(factory, IVaultFactory::predictVaultCall { strategyId: id })
        .await?;
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
    tracing::info!(%vault, tx = %receipt.transaction_hash, "activated");
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
    tracing::info!(%vault, %who, %amount, tx = %receipt.transaction_hash, "deposit requested");
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

/// Ask for all of `who`'s shares back and follow the claim it books until
/// it is paid (staged at the station, proved, swept, delivered, then paid at
/// a valuation), for up to `minutes`.
pub async fn claim_all(client: &Client, vault: Address, who: Address, minutes: u64) -> Result<()> {
    let shares = client
        .read(vault, IVault::balanceOfCall { owner: who })
        .await?;
    let claims_before = client.read(vault, IVault::claimCountCall {}).await?;
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
    tracing::info!(%vault, %shares, tx = %receipt.transaction_hash, "withdrawal requested");
    let claim_id = claims_before + U256::from(1u64);
    for _ in 0..minutes * 12 {
        if client.read(vault, IVault::claimCountCall {}).await? >= claim_id {
            let claim = client
                .read(vault, IVault::claimAtCall { claimId: claim_id })
                .await?;
            if matches!(claim.state, ClaimState::Paid) {
                tracing::info!(%vault, paid = %claim.paid, "the claim is paid");
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    bail!("the claim was not paid in {minutes} minutes")
}

/// The whole run on a testnet whose services read [`adopted`]: deploy (or
/// find) the stack; publish `agent` through the API on `api`; activate it at
/// the Hyperliquid factory; a depositor's USDC in; once it is priced, the
/// station's HyperCore account funded with `station_dollars` so the agent
/// has a venue balance to trade; then, after `trade_minutes`, all of the
/// depositor's shares asked back and the claim followed until it is paid.
///
/// What an agent can do depends on the vault's size: `mm-quoter` quotes 2%
/// of the vault's value a side and posts nothing under $12, so it quotes
/// only once the vault is worth $600 or more.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    client: &Client,
    artifacts: &Path,
    deployments: &Deployments,
    deployments_dir: &Path,
    api: &str,
    agent: &Path,
    station_dollars: f64,
    trade_minutes: u64,
) -> Result<()> {
    let file = HyperliquidVaults::file_in(deployments_dir);
    let vaults = ensure(client, artifacts, deployments, &file).await?;
    let author = client.account(Role::Author)?;
    let depositor = client.account(Role::Viewer)?;
    let funder = Funder::new(client)?;

    let published = crate::publish::publish(
        client,
        shared::Chain::Hyperliquid,
        deployments,
        api,
        agent,
        author,
        false,
        false,
    )
    .await?;
    let strategy_id = published
        .strategy_id
        .ok_or_else(|| anyhow!("the agent was not registered: {:?}", published.outcome))?;
    tracing::info!(strategy_id, outcome = ?published.outcome, "published");

    let minimum_tank = client
        .read(vaults.factory, IVaultFactory::configCall {})
        .await?;
    let tank_dollars = minimum_tank.minimumTank.to::<u64>() as f64 / 1e6;
    let deposit_units = minimum_tank.minimumDeposit.to::<u64>().max(20_000_000);
    funder
        .dollars_on_hyperevm(client, author, tank_dollars)
        .await?;
    funder
        .dollars_on_hyperevm(client, depositor, deposit_units as f64 / 1e6)
        .await?;

    // The review is posted by the reviewer service; activation waits for it.
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
    deposit(client, &vaults, vault, depositor, deposit_units).await?;
    let shares = shares_of(client, vault, depositor, 15).await?;
    tracing::info!(%vault, %shares, "the deposit is priced");

    // Funded once the depositor holds the shares, so what the account
    // brings is theirs rather than the dead shares'. The executor's agent
    // approval is queued at the venue and dropped while the account doesn't
    // exist; it approves again within a minute once it reads it missing.
    let station = client.read(vault, IVault::homeStationCall {}).await?;
    // HYPE for the account's sends home: a send costs a tiny fraction of
    // this, and the funder's own spot HYPE is what `chains:fund` left.
    funder
        .station_account(station, station_dollars, 0.001)
        .await?;
    tokio::time::sleep(Duration::from_secs(trade_minutes * 60)).await;
    claim_all(client, vault, depositor, 30).await
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
        };
        let text = serde_json::to_string(&vaults).unwrap();
        assert_eq!(
            serde_json::from_str::<HyperliquidVaults>(&text).unwrap(),
            vaults
        );
        assert!(HyperliquidVaults::file_in(Path::new("/x")).ends_with("hyperliquid-998.json"));
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
        };
        let adopted = adopted(&deployments, &vaults);
        assert_eq!(adopted.factory, vaults.factory);
        assert_eq!(adopted.station_factory, vaults.station_factory);
        // The publishing fee is still paid in the deployment's own dollar.
        assert_eq!(adopted.usd, deployments.usd);
        assert_eq!(adopted.deployed_block, deployments.deployed_block);
    }
}
