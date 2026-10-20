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

use alloy::network::TransactionBuilder;
use alloy::primitives::utils::{format_ether, parse_ether};
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, anyhow, bail};
use chain::{ChainConfig, ChainKey, NetworkName, Role};
use hyperliquid::{Api, HyperCore};
use serde::Deserialize;

/// The funder's index along `m/44'/60'/0'/0/<i>` of the EVM mnemonic: fixed,
/// and never one of the protocol's roles.
pub const FUNDER_INDEX: u32 = 100;

/// The funding file's name for `testnet-market`'s account: an account of
/// the EVM mnemonic of its own ([`testnet_market::MARKET_INDEX`]), never
/// one of the platform's roles.
pub const MARKET: &str = "market";

/// `testnet-market`'s account on the test networks.
pub fn market_account() -> Result<Address> {
    Ok(mnemonic_key(testnet_market::MARKET_INDEX)?.address())
}

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
                if name != MARKET && !ROLES.iter().any(|role| role.label() == name) {
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

    /// What `testnet-market`'s account is topped up to on a chain: its own
    /// amount, never the default (a chain without one has no market to
    /// keep).
    pub fn market_target(&self, chain: &ChainKey) -> Option<U256> {
        coin(*self.testnet.get(&chain.to_string())?.roles.get(MARKET)?).ok()
    }

    /// What `testnet-market`'s Solana account is topped up to on a cluster,
    /// in lamports: its own amount, never the default.
    pub fn market_lamports(&self, cluster: &ChainKey) -> Result<Option<u64>> {
        self.testnet
            .get(&cluster.to_string())
            .and_then(|amounts| amounts.roles.get(MARKET))
            .map(|amount| lamports(*amount))
            .transpose()
    }

    /// What a person's own wallet is topped up to on an EVM chain
    /// (`fund-wallet --gas`): the chain's `default`, if it is listed.
    pub fn wallet_coin(&self, chain: &ChainKey) -> Option<U256> {
        coin(self.testnet.get(&chain.to_string())?.default).ok()
    }

    /// The same on a Solana cluster, in lamports.
    pub fn wallet_lamports(&self, cluster: &ChainKey) -> Result<Option<u64>> {
        self.testnet
            .get(&cluster.to_string())
            .map(|amounts| lamports(amounts.default))
            .transpose()
    }

    /// What the funder keeps for itself on a chain.
    pub fn reserve(&self, chain: &ChainKey) -> Option<U256> {
        coin(self.testnet.get(&chain.to_string())?.reserve).ok()
    }

    /// On a Solana cluster, where the deployer is the funder: what it keeps
    /// and what each role is topped up to. `None` if the cluster isn't
    /// listed.
    pub fn solana(&self, cluster: &ChainKey) -> Result<Option<SolanaAmounts>> {
        let Some(amounts) = self.testnet.get(&cluster.to_string()) else {
            return Ok(None);
        };
        let reserve = lamports(amounts.reserve)?;
        let roles = ROLES
            .iter()
            .map(|role| {
                let amount = amounts
                    .roles
                    .get(role.label())
                    .copied()
                    .unwrap_or(amounts.default);
                Ok((*role, lamports(amount)?))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(SolanaAmounts { reserve, roles }))
    }
}

/// A Solana cluster's amounts, in lamports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolanaAmounts {
    /// What the deployer keeps for itself: its own set-up's rent and fees
    /// come out of it.
    pub reserve: u64,
    /// What each role is topped up to, in [`ROLES`]' order.
    pub roles: Vec<(Role, u64)>,
}

fn coin(amount: f64) -> Result<U256> {
    parse_ether(&format!("{amount}")).map_err(|error| anyhow!("{amount}: {error}"))
}

/// An amount of SOL as lamports, exactly as written: an amount under the
/// least an account may hold (rent-exempt, about 0.00089 SOL) is refused,
/// since a fresh account given it would be refused by the cluster; zero
/// funds nothing.
fn lamports(amount: f64) -> Result<u64> {
    const LEAST: u64 = 1_000_000;
    let parsed = alloy::primitives::utils::parse_units(&format!("{amount}"), 9)
        .map_err(|error| anyhow!("{amount} SOL: {error}"))?;
    if parsed.is_negative() {
        bail!("{amount} SOL is not an amount a role can be given");
    }
    let lamports = u64::try_from(parsed.get_absolute())
        .map_err(|_| anyhow!("{amount} SOL is not an amount a role can be given"))?;
    if lamports != 0 && lamports < LEAST {
        bail!("{amount} SOL is less than an account may hold (0.001 SOL or more, or 0)");
    }
    Ok(lamports)
}

/// The funder's key, from the EVM mnemonic.
pub fn funder() -> Result<PrivateKeySigner> {
    mnemonic_key(FUNDER_INDEX)
}

/// The EVM mnemonic's key at `index`.
fn mnemonic_key(index: u32) -> Result<PrivateKeySigner> {
    use alloy::signers::local::MnemonicBuilder;
    use alloy::signers::local::coins_bip39::English;
    let phrase = std::env::var(chain::args::TESTNET_EVM_MNEMONIC)
        .ok()
        .filter(|phrase| !phrase.trim().is_empty())
        .ok_or_else(|| {
            anyhow!(
                "the funder and the market's account come from {}; set it in .env",
                chain::args::TESTNET_EVM_MNEMONIC
            )
        })?;
    MnemonicBuilder::<English>::default()
        .phrase(phrase.trim())
        .index(index)
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
    // Every chain is funded and reported, whatever became of the others,
    // the chains at once: the funder's sends are numbered per chain.
    let outcomes = futures::future::join_all(chains.into_iter().map(|(key, config)| {
        let (funding, funder) = (&funding, &funder);
        async move {
            let outcome = fund_chain(funding, funder, network, &key, &config).await;
            if let Err(error) = &outcome {
                tracing::error!(chain = %key, error = format!("{error:#}"), "funding stopped");
            }
            (key, config.label, outcome)
        }
    }))
    .await;
    println!("Funding:");
    let mut failed = Vec::new();
    for (key, label, outcome) in outcomes {
        match outcome {
            Ok(()) => println!("  {label} ({key}): done"),
            Err(error) => {
                println!("  {label} ({key}): failed: {error:#}");
                failed.push(key.to_string());
            }
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
    // Through the client's own connection: an endpoint's URL may carry a
    // key, and an error must never name it.
    let provider = client.signing_provider(funder.clone())?;
    let from = funder.address();
    println!("{} ({key}), funder {from:?}", config.label);

    // On HyperEVM, HYPE is bought on HyperCore first. If that fails, the
    // roles are still topped up from what the funder already holds here,
    // and the failure is the chain's outcome.
    let mut prepared = Ok(());
    if let Some(api) = &Api::for_chain(client.chain_id) {
        prepared = prepare_hyperevm(funding, funder, api, &client, &provider, key, reserve).await;
        if let Err(error) = &prepared {
            println!("  HyperCore: {error:#}; topping up from what the funder holds here");
        }
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
    // testnet-market's account, on the chains it keeps a pool on.
    if let Some(target) = funding.market_target(key) {
        let address = market_account()?;
        let balance = provider.get_balance(address).await?;
        let need = target.saturating_sub(balance);
        if need.is_zero() {
            println!(
                "  {MARKET:<9} {address:?} holds {}, enough",
                format_ether(balance)
            );
        } else if need > available {
            println!(
                "  {MARKET:<9} {address:?} holds {}, short {}: the funder can't cover it past its reserve",
                format_ether(balance),
                format_ether(need)
            );
        } else {
            let hash = send_value(&provider, from, address, need).await?;
            println!(
                "  {MARKET:<9} {address:?} sent {} ({hash})",
                format_ether(need)
            );
        }
    }
    prepared
}

/// Send the chain's coin from the funder at a legacy gas price (HyperEVM's
/// public RPC takes nothing else), and wait for it to land.
pub(crate) async fn send_value<P: Provider>(
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
    let short = shortfalls(funding, client, key).await?;
    let wanted = short.total + reserve;
    if held >= wanted {
        return Ok(());
    }
    let missing = to_tokens(wanted - held);
    // Whether a role is short more than dust of its amount: only then is a
    // purchase under the venue's smallest order rounded up to it.
    let needed = short.rows.iter().any(|(role, _, _, need)| {
        let target = funding.target(key, *role).unwrap_or(U256::ZERO);
        is_more_than_dust(*need, target)
    });
    let hype = api.spot_token("HYPE").await?;
    let pair = hype
        .usdc_pair
        .ok_or_else(|| anyhow!("HYPE has no USDC pair on HyperCore"))?;
    let on_core = api.spot_balance(from, "HYPE").await?;
    let step = 10f64.powi(-(hype.sz_decimals as i32));
    let wanted_on_core = round_up((missing - on_core).max(0.0) * 1.01, hype.sz_decimals);
    let mut to_buy = 0.0;
    if wanted_on_core >= step {
        let (_, ask) = api.best_bid_ask(&format!("@{pair}")).await?;
        let ask = ask.ok_or_else(|| anyhow!("nobody is selling HYPE on HyperCore"))?;
        if ask > settings.max_price {
            bail!(
                "HYPE's best ask is ${ask}, over the ${} the funding file allows",
                settings.max_price
            );
        }
        match hype_purchase(wanted_on_core, ask, hype.sz_decimals, needed) {
            Purchase::Size(size) => to_buy = size,
            Purchase::Dust { worth } => println!(
                "  funder: the roles are short only dust (${worth:.2} of HYPE, under the venue's ${MIN_ORDER_USD} smallest order): left as it is"
            ),
        }
    }
    if to_buy > settings.max_per_run {
        bail!(
            "the roles need {to_buy} HYPE, over the {} a run may buy: raise max_per_run in the funding file, or run it again",
            settings.max_per_run
        );
    }
    if to_buy >= step {
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
        if to_buy < step {
            // Nothing bought (dust), and nothing on HyperCore to send.
            return Ok(());
        }
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

/// The venue's smallest order, in dollars: it refuses an order worth less
/// ("Order must have minimum value of 10 USDC").
const MIN_ORDER_USD: f64 = 10.0;
/// How far over the smallest order a purchase rounded up to it goes, so a
/// price moving before it is placed does not take it under.
const MIN_ORDER_MARGIN: f64 = 1.05;

/// Whether a role short `need` of its amount `target` is short more than
/// dust: more than a quarter of its amount. A role short less holds nearly
/// its amount.
fn is_more_than_dust(need: U256, target: U256) -> bool {
    need.saturating_mul(U256::from(4u8)) > target
}

/// What to buy on HyperCore, in HYPE.
#[derive(Debug, PartialEq)]
enum Purchase {
    /// This many, never worth less than the venue's smallest order.
    Size(f64),
    /// Nothing: what is missing is worth less than the smallest order and
    /// no role is short more than dust, so it is left.
    Dust { worth: f64 },
}

/// A purchase of `wanted` HYPE (already rounded to the token's size
/// decimals) at about `ask` dollars a HYPE, as the venue will take it: at
/// least [`MIN_ORDER_USD`] × [`MIN_ORDER_MARGIN`] at the ask. Under that, it
/// is rounded up to it if `needed` (a role is short more than dust; the
/// extra stays with the funder for later runs), and otherwise left.
fn hype_purchase(wanted: f64, ask: f64, sz_decimals: u32, needed: bool) -> Purchase {
    let least = MIN_ORDER_USD * MIN_ORDER_MARGIN;
    let worth = wanted * ask;
    if worth >= least {
        Purchase::Size(wanted)
    } else if needed {
        Purchase::Size(round_up(least / ask, sz_decimals))
    } else {
        Purchase::Dust { worth }
    }
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
    fn a_wallet_is_topped_up_to_its_chain_s_default_and_an_unlisted_chain_gives_nothing() {
        let funding: Funding = toml::from_str(FILE).unwrap();
        let key: ChainKey = "evm:84532".parse().unwrap();
        assert_eq!(
            funding.wallet_coin(&key),
            Some(parse_ether("0.004").unwrap())
        );
        let other: ChainKey = "evm:1".parse().unwrap();
        assert_eq!(funding.wallet_coin(&other), None);
        let cluster: ChainKey = "solana:devnet".parse().unwrap();
        assert_eq!(funding.wallet_lamports(&cluster).unwrap(), None);
        let with_cluster: Funding = toml::from_str(&format!(
            "{FILE}\n[testnet.\"solana:devnet\"]\nreserve = 1\ndefault = 0.2\n"
        ))
        .unwrap();
        assert_eq!(
            with_cluster.wallet_lamports(&cluster).unwrap(),
            Some(200_000_000)
        );
    }

    #[test]
    fn a_price_keeps_five_significant_figures_rounded_down() {
        assert_eq!(round_price(23.6355), 23.635);
        assert_eq!(round_price(0.0287114), 0.028711);
        assert_eq!(round_up(1.001, 2), 1.01);
        assert_eq!(round_down(1.009, 2), 1.0);
    }

    #[test]
    fn a_purchase_is_never_under_the_venue_s_smallest_order() {
        // Enough on its own.
        assert_eq!(hype_purchase(1.0, 35.0, 2, false), Purchase::Size(1.0));
        // Dust the roles can do without: left.
        assert!(matches!(
            hype_purchase(0.01, 35.0, 2, false),
            Purchase::Dust { worth } if (worth - 0.35).abs() < 1e-9
        ));
        // Short more than dust: rounded up to the smallest order and a
        // margin, at the token's size decimals.
        let Purchase::Size(size) = hype_purchase(0.01, 35.0, 2, true) else {
            panic!("a needed purchase is made");
        };
        assert_eq!(size, 0.3);
        assert!(size * 35.0 >= MIN_ORDER_USD);
        for ask in [0.5, 3.0, 12.6, 35.0, 59.9] {
            for wanted in [0.0001, 0.01, 0.2, 5.0] {
                if let Purchase::Size(size) = hype_purchase(wanted, ask, 2, true) {
                    assert!(size * ask >= MIN_ORDER_USD, "{size} at {ask}");
                    assert!(size >= wanted);
                }
            }
        }
    }

    #[test]
    fn a_role_short_a_quarter_of_its_amount_or_less_holds_nearly_its_amount() {
        let target = parse_ether("0.4").unwrap();
        assert!(!is_more_than_dust(parse_ether("0.1").unwrap(), target));
        assert!(!is_more_than_dust(U256::ZERO, target));
        assert!(is_more_than_dust(parse_ether("0.11").unwrap(), target));
        assert!(is_more_than_dust(parse_ether("0.01").unwrap(), U256::ZERO));
    }

    #[test]
    fn the_market_account_is_funded_only_where_the_file_names_it() {
        let funding: Funding = toml::from_str(&format!(
            "{FILE}\n[testnet.\"evm:46630\"]\nreserve = 0.001\ndefault = 0.0003\nmarket = 0.005\n"
        ))
        .unwrap();
        let robinhood: ChainKey = "evm:46630".parse().unwrap();
        let base: ChainKey = "evm:84532".parse().unwrap();
        assert_eq!(
            funding.market_target(&robinhood),
            Some(parse_ether("0.005").unwrap())
        );
        assert_eq!(funding.market_target(&base), None, "not the default");
    }

    #[test]
    fn the_shipped_funding_file_names_only_roles() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/funding.toml");
        Funding::read(&path).unwrap();
    }

    #[test]
    fn a_solana_cluster_s_roles_are_funded_in_lamports_exactly_as_written() {
        let funding: Funding = toml::from_str(
            r#"
[testnet."solana:devnet"]
reserve = 2
default = 0.05
keeper = 1.5
executor = 0
attestor = 0.003

[hyperliquid]
max_price = 35.0
max_per_run = 3.0
activation_usd = 2.0
"#,
        )
        .unwrap();
        let devnet: ChainKey = "solana:devnet".parse().unwrap();
        let SolanaAmounts { reserve, roles } = funding.solana(&devnet).unwrap().unwrap();
        assert_eq!(reserve, 2_000_000_000);
        assert_eq!(roles.len(), ROLES.len());
        let of = |role: Role| roles.iter().find(|(r, _)| *r == role).unwrap().1;
        assert_eq!(of(Role::Keeper), 1_500_000_000);
        assert_eq!(of(Role::Executor), 0);
        assert_eq!(of(Role::Attestor), 3_000_000);
        assert_eq!(of(Role::Viewer), 50_000_000);
        let other: ChainKey = "solana:mainnet-beta".parse().unwrap();
        assert!(funding.solana(&other).unwrap().is_none());
    }

    #[test]
    fn a_solana_amount_under_what_an_account_may_hold_is_refused() {
        assert_eq!(lamports(0.0).unwrap(), 0);
        assert_eq!(lamports(0.001).unwrap(), 1_000_000);
        assert!(lamports(0.0005).is_err());
        assert!(lamports(-1.0).is_err());
    }

    #[test]
    fn the_shipped_funding_file_funds_solana_devnet_by_transfer() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/funding.toml");
        let devnet: ChainKey = "solana:devnet".parse().unwrap();
        let SolanaAmounts { reserve, roles } = Funding::read(&path)
            .unwrap()
            .solana(&devnet)
            .unwrap()
            .expect("solana:devnet is in the funding file");
        assert!(reserve > 0);
        // The keys that sign the set-up and keep the vaults are funded.
        for role in [Role::Admin, Role::Operator, Role::Keeper] {
            assert!(roles.iter().any(|(r, amount)| *r == role && *amount > 0));
        }
    }
}
