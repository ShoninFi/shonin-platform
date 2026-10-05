//! Funding the testnet role accounts from one funder per chain.
//!
//! The owner funds two addresses: the EVM funder (`m/44'/60'/0'/0/100` of the
//! EVM mnemonic, on every EVM testnet) and the Solana deployer. This tops
//! each EVM role up to the amount `config/funding.toml` gives it, from the
//! funder, and leaves alone a role that already holds it, so running it again
//! sends nothing new. It refuses every network but the testnets.
//!
//! On Hyperliquid's testnet the funder's money is dollars on HyperCore, not
//! HYPE on HyperEVM: what the roles need is bought on HyperCore's spot market
//! and sent across to HyperEVM first. The deploying account is brought into
//! being on HyperCore and switched to big blocks, since some contracts are
//! larger than a small block holds.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::utils::{format_ether, parse_ether};
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, anyhow, bail};
use chain::{ChainConfig, ChainKey, NetworkName, Role};
use hyperliquid::{Api, HyperCore};
use serde::Deserialize;

/// The funder's index along `m/44'/60'/0'/0/<i>` of the EVM mnemonic: fixed,
/// and never one of the protocol's roles.
pub const FUNDER_INDEX: u32 = 100;

/// Every role, in the order they're listed.
pub const ROLES: [Role; 10] = [
    Role::Admin,
    Role::Builder,
    Role::Reviewer,
    Role::Executor,
    Role::Keeper,
    Role::Author,
    Role::Viewer,
    Role::Operator,
    Role::Attestor,
    Role::Platform,
];

/// `config/funding.toml`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Funding {
    #[serde(default)]
    testnet: BTreeMap<String, Amounts>,
    hyperliquid: HyperliquidFunding,
}

/// What each role on one chain is topped up to, in the chain's coin.
#[derive(Deserialize)]
pub struct Amounts {
    reserve: f64,
    default: f64,
    #[serde(flatten)]
    roles: BTreeMap<String, f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HyperliquidFunding {
    max_price: f64,
    max_per_run: f64,
    activation_usd: f64,
}

impl Funding {
    pub fn read(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let funding: Self =
            toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
        for amounts in funding.testnet.values() {
            for name in amounts.roles.keys() {
                if !ROLES.iter().any(|role| role.label() == name) {
                    bail!("{} names {name:?}, which is not a role", path.display());
                }
            }
        }
        Ok(funding)
    }

    /// What a role on a chain is topped up to, if the chain is listed.
    pub fn target(&self, chain: &ChainKey, role: Role) -> Option<U256> {
        let amounts = self.testnet.get(&chain.to_string())?;
        let amount = amounts
            .roles
            .get(role.label())
            .copied()
            .unwrap_or(amounts.default);
        coin(amount).ok()
    }

    /// What the funder keeps for itself on a chain.
    pub fn reserve(&self, chain: &ChainKey) -> Option<U256> {
        coin(self.testnet.get(&chain.to_string())?.reserve).ok()
    }
}

fn coin(amount: f64) -> Result<U256> {
    parse_ether(&format!("{amount}")).map_err(|error| anyhow!("{amount}: {error}"))
}

/// The funder's key, from the EVM mnemonic.
pub fn funder() -> Result<PrivateKeySigner> {
    use alloy::signers::local::MnemonicBuilder;
    use alloy::signers::local::coins_bip39::English;
    let phrase = std::env::var(chain::args::TESTNET_EVM_MNEMONIC)
        .ok()
        .filter(|phrase| !phrase.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "the funder comes from {}; set it in .env",
                chain::args::TESTNET_EVM_MNEMONIC
            )
        })?;
    MnemonicBuilder::<English>::default()
        .phrase(phrase.trim())
        .index(FUNDER_INDEX)
        .ok()
        .and_then(|builder| builder.build().ok())
        .ok_or_else(|| anyhow!("the EVM mnemonic is not a valid BIP-39 phrase"))
}

/// A role's own key, from the EVM mnemonic, for what it signs on HyperCore.
fn role_key(role: Role) -> Result<PrivateKeySigner> {
    let phrase = std::env::var(chain::args::TESTNET_EVM_MNEMONIC).unwrap_or_default();
    let mut keys = chain::keys_from_mnemonic(&phrase, chain::ROLE_COUNT)?;
    Ok(keys.swap_remove(role.account_index()))
}

/// One EVM chain's roles: who they are, what they hold and what they're short
/// of their amount.
pub struct Shortfalls {
    pub rows: Vec<(Role, Address, U256, U256)>,
    pub total: U256,
}

pub async fn shortfalls(
    funding: &Funding,
    client: &chain::Client,
    key: &ChainKey,
) -> Result<Shortfalls> {
    let mut rows = Vec::new();
    let mut total = U256::ZERO;
    for role in ROLES {
        let address = client.account(role)?;
        let balance = client.balance(address).await?;
        let target = funding.target(key, role).unwrap_or(U256::ZERO);
        let short = target.saturating_sub(balance);
        total += short;
        rows.push((role, address, balance, short));
    }
    Ok(Shortfalls { rows, total })
}

/// Top every role up on every EVM testnet, then say what was sent.
pub async fn run(
    network: NetworkName,
    chains: Vec<(ChainKey, ChainConfig)>,
    funding_file: &Path,
) -> Result<()> {
    if network != NetworkName::Testnet {
        bail!(
            "chains:fund tops up the testnet role accounts from the funder, and runs only with NETWORK=testnet (the local chains fund their own accounts; mainnet has no role keys)"
        );
    }
    let funding = Funding::read(funding_file)?;
    let funder = funder()?;
    let mut failed = Vec::new();
    for (key, config) in chains {
        if let Err(error) = fund_chain(&funding, &funder, network, &key, &config).await {
            tracing::error!(chain = %key, error = format!("{error:#}"), "funding stopped");
            failed.push(key.to_string());
        }
    }
    if !failed.is_empty() {
        bail!("funding failed on {}", failed.join(", "));
    }
    Ok(())
}

async fn fund_chain(
    funding: &Funding,
    funder: &PrivateKeySigner,
    network: NetworkName,
    key: &ChainKey,
    config: &ChainConfig,
) -> Result<()> {
    let Some(reserve) = funding.reserve(key) else {
        println!("{} ({key}): not in the funding file, skipped", config.label);
        return Ok(());
    };
    let client = chain::connect_to(network, key, config).await?;
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(funder.clone()))
        .connect_http(config.first_rpc().parse().context("the chain's RPC URL")?);
    let from = funder.address();
    println!("{} ({key}), funder {from:?}", config.label);

    let hypercore = Api::for_chain(client.chain_id);
    if let Some(api) = &hypercore {
        prepare_hyperevm(funding, funder, api, &client, &provider, key, reserve).await?;
    }

    let short = shortfalls(funding, &client, key).await?;
    let mut available = provider.get_balance(from).await?.saturating_sub(reserve);
    for (role, address, balance, need) in short.rows {
        if need.is_zero() {
            println!(
                "  {:<9} {address:?} holds {}, enough",
                role.label(),
                format_ether(balance)
            );
            continue;
        }
        if need > available {
            println!(
                "  {:<9} {address:?} holds {}, short {}: the funder can't cover it past its reserve",
                role.label(),
                format_ether(balance),
                format_ether(need)
            );
            continue;
        }
        let hash = send_value(&provider, from, address, need).await?;
        available -= need;
        println!(
            "  {:<9} {address:?} sent {} ({hash})",
            role.label(),
            format_ether(need)
        );
    }
    Ok(())
}

/// Send the chain's coin from the funder at a legacy gas price (HyperEVM's
/// public RPC takes nothing else), and wait for it to land.
async fn send_value<P: Provider>(
    provider: &P,
    from: Address,
    to: Address,
    value: U256,
) -> Result<String> {
    let price = provider.get_gas_price().await?;
    let request = TransactionRequest::default()
        .with_from(from)
        .with_to(to)
        .with_value(value)
        .with_gas_price(price + price / 5 + 1);
    let receipt = provider
        .send_transaction(request)
        .await
        .context("sending from the funder")?
        .with_timeout(Some(Duration::from_secs(120)))
        .get_receipt()
        .await
        .context("waiting for the funder's send to land")?;
    if !receipt.status() {
        bail!("the send to {to} reverted");
    }
    Ok(format!("{:#x}", receipt.transaction_hash))
}

/// On HyperEVM: bring the deploying account into being on HyperCore and put
/// it on big blocks, then buy on HyperCore whatever HYPE the roles and the
/// reserve need beyond what the funder already holds on HyperEVM, and send it
/// across.
async fn prepare_hyperevm<P: Provider>(
    funding: &Funding,
    funder: &PrivateKeySigner,
    api: &Api,
    client: &chain::Client,
    provider: &P,
    key: &ChainKey,
    reserve: U256,
) -> Result<()> {
    let settings = &funding.hyperliquid;
    let core = HyperCore::new(api.clone(), funder.clone());
    let from = funder.address();

    // The deployer acts on HyperCore (big blocks), so it must exist there.
    let admin = client.account(Role::Admin)?;
    if !api.account_exists(admin).await? {
        core.usd_send(admin, settings.activation_usd)
            .await
            .context("bringing the deployer into being on HyperCore")?;
        println!(
            "  admin     {admin:?} given ${} on HyperCore, which brings it into being there",
            settings.activation_usd
        );
        wait_until(|| async { api.account_exists(admin).await.unwrap_or(false) }).await?;
    }
    let deployer = role_key(Role::Admin)?;
    if deployer.address() != admin {
        bail!("the deployer's key is not the admin account the chain client uses");
    }
    HyperCore::new(api.clone(), deployer)
        .use_big_blocks(true)
        .await
        .context("switching the deployer to big blocks")?;
    println!("  admin     {admin:?} on big blocks, for contracts over 2M gas");

    let held = provider.get_balance(from).await?;
    let wanted = shortfalls(funding, client, key).await?.total + reserve;
    if held >= wanted {
        return Ok(());
    }
    let missing = to_tokens(wanted - held);
    let hype = api.spot_token("HYPE").await?;
    let pair = hype
        .usdc_pair
        .ok_or_else(|| anyhow!("HYPE has no USDC pair on HyperCore"))?;
    let on_core = api.spot_balance(from, "HYPE").await?;
    let step = 10f64.powi(-(hype.sz_decimals as i32));
    let to_buy = round_up((missing - on_core).max(0.0) * 1.01, hype.sz_decimals);
    if to_buy > settings.max_per_run {
        bail!(
            "the roles need {to_buy} HYPE, over the {} a run may buy: raise max_per_run in the funding file, or run it again",
            settings.max_per_run
        );
    }
    if to_buy >= step {
        let (_, ask) = api.best_bid_ask(&format!("@{pair}")).await?;
        let ask = ask.ok_or_else(|| anyhow!("nobody is selling HYPE on HyperCore"))?;
        if ask > settings.max_price {
            bail!(
                "HYPE's best ask is ${ask}, over the ${} the funding file allows",
                settings.max_price
            );
        }
        // The order may sweep the book up to the ceiling: testnet books are
        // thin, and a fill only at the best ask often covers a fraction.
        let limit = round_price(settings.max_price);
        let cost = to_buy * limit * 1.01;
        let spot_usdc = api.spot_balance(from, "USDC").await?;
        if spot_usdc < cost {
            let amount = (cost - spot_usdc).ceil() + 1.0;
            core.usd_class_transfer(amount, false)
                .await
                .context("moving dollars from perp to spot")?;
            println!("  funder moved ${amount} from perp to spot on HyperCore");
        }
        let fill = core.spot_ioc(pair, true, to_buy, limit).await?;
        println!(
            "  funder bought {} HYPE at ${} on HyperCore",
            fill.size, fill.average_price
        );
    }
    let on_core = api.spot_balance(from, "HYPE").await?;
    let send = round_down(on_core.min(missing * 1.01), 8);
    if send <= 0.0 {
        bail!("no HYPE on HyperCore to send to HyperEVM");
    }
    core.spot_send(hyperliquid::api::system_address(&hype), &hype, send)
        .await
        .context("sending HYPE to HyperEVM")?;
    println!("  funder sent {send} HYPE from HyperCore to HyperEVM");
    wait_until(|| async {
        provider
            .get_balance(from)
            .await
            .map(|now| now > held)
            .unwrap_or(false)
    })
    .await
    .context("waiting for the HYPE to reach HyperEVM")
}

async fn wait_until<F, Fut>(mut check: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..60 {
        if check().await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    bail!("not seen within two minutes")
}

fn to_tokens(amount: U256) -> f64 {
    format_ether(amount).parse().unwrap_or(f64::MAX)
}

fn round_up(value: f64, decimals: u32) -> f64 {
    let scale = 10f64.powi(decimals as i32);
    (value * scale).ceil() / scale
}

fn round_down(value: f64, decimals: u32) -> f64 {
    let scale = 10f64.powi(decimals as i32);
    (value * scale).floor() / scale
}

/// A price as HyperCore takes it: at most five significant figures.
fn round_price(price: f64) -> f64 {
    if price <= 0.0 {
        return 0.0;
    }
    let magnitude = price.log10().floor() as i32;
    let scale = 10f64.powi(4 - magnitude);
    (price * scale).floor() / scale
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"
[testnet."evm:84532"]
reserve = 0.01
default = 0.004
admin = 0.03

[hyperliquid]
max_price = 35.0
max_per_run = 3.0
activation_usd = 2.0
"#;

    #[test]
    fn a_role_not_named_is_topped_up_to_the_default() {
        let funding: Funding = toml::from_str(FILE).unwrap();
        let key: ChainKey = "evm:84532".parse().unwrap();
        assert_eq!(
            funding.target(&key, Role::Admin),
            Some(parse_ether("0.03").unwrap())
        );
        assert_eq!(
            funding.target(&key, Role::Viewer),
            Some(parse_ether("0.004").unwrap())
        );
        let other: ChainKey = "evm:1".parse().unwrap();
        assert_eq!(funding.target(&other, Role::Admin), None);
    }

    #[test]
    fn a_price_keeps_five_significant_figures_rounded_down() {
        assert_eq!(round_price(23.6355), 23.635);
        assert_eq!(round_price(0.0287114), 0.028711);
        assert_eq!(round_up(1.001, 2), 1.01);
        assert_eq!(round_down(1.009, 2), 1.0);
    }

    #[test]
    fn the_shipped_funding_file_names_only_roles() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/funding.toml");
        Funding::read(&path).unwrap();
    }
}
