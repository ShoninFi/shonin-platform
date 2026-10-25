//! What `testnet-market` needs on the test networks, set up from the
//! funder: the mirror feeds, the market's account's gas and inventory, the
//! pools' depth, and the Robinhood driver built from the listings.
//!
//! Each step but `pools` is safe to run again: a mirror already recorded
//! and on the chain is kept, and an account holding its amount is left
//! alone; `pools` adds its liquidity each time it runs. Test networks only.
//!
//! - `feeds`: one mirror feed per listed stock whose price comes from a
//!   mainnet feed, deployed by the funder, posted to by the market's account
//!   alone, starting from the mainnet feed's latest round; their addresses
//!   go in `mirror-feeds-evm-<chain>.json` beside the chain's deployment
//!   file, where the market and the driver's deploy read them.
//! - `gas`: the market's account topped up to `--gas`, or to the funding
//!   file's `market` amount, never past the funder's reserve.
//! - `inventory`: the market's account topped up to `--stock-tokens` of
//!   each stock token, `--weth` of WETH (wrapped from the funder's coin) and
//!   `--usdc` of USDC, from what the funder holds. A dollar anyone may mint
//!   is minted by the market as it needs it.
//! - `pools`: `--pool-tokens` of each listed token added to its pool as a
//!   full-range position, with the dollar at the pool's own price (minted
//!   where anyone may mint it, else the funder's).
//! - `driver`: the Robinhood driver deployed by the admin from the listings
//!   resolved against the recorded mirrors, recorded in
//!   `robinhood-driver-evm-<chain>.json`, and with `--propose` proposed to
//!   the chain's registry under the name `config/drivers.toml` lists for
//!   that chain: `robinhood` on Robinhood Chain, `uniswap-base` on Base.
//!
//! On Solana devnet the market's own Solana account
//! ([`testnet_market::SOLANA_MARKET_INDEX`]) keeps Orca's pool:
//!
//! - `gas`: the account topped up to `--sol`, or the funding file's
//!   `market` amount, from the deployer (Solana's funder), never past its
//!   reserve. Its SOL is also the SOL side of what it trades.
//! - `inventory`: what it holds, and the faucet to tap: its USDC is
//!   Circle's devnet USDC, which only Circle's faucet gives.
//! - `pools`: the pool the Orca driver lists created at Pyth's price if it
//!   isn't there, walked to Pyth's price with the account's own tokens, and
//!   the account's seed opened across it with `--orca-usdc` of USDC and as
//!   much SOL (once: a seed already open is left alone).

use std::path::Path;
use std::time::Duration;

use alloy::network::TransactionBuilder;
use alloy::primitives::utils::{format_ether, format_units, parse_ether, parse_units};
use alloy::primitives::{Address, Bytes, U256, aliases::I24, aliases::U24};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use alloy::sol_types::{SolCall, SolConstructor};
use anyhow::{Context, Result, anyhow, bail};
use chain::bindings::attestation::IAttestationRegistry;
use chain::{ChainConfig, ChainConfigs, ChainKey, ChainKind, Client, NetworkName, Role};
use robinhood::listings::{
    self, FEED_DECIMALS, Feed, MAX_LISTINGS, MODULE_PROXY_FACTORY, MirrorFeeds, MirrorRecord,
    ROLES_MASTERCOPY, Resolved, SAFE_PROXY_FACTORY, SAFE_SINGLETON, Venue,
};
use serde::{Deserialize, Serialize};
use testnet_market::bindings::{AggregatorV3Interface, IMirrorFeed, IToken, IUniswapV3Pool};

use crate::deploy::{creation_code, market_id};
use crate::fund::{self, Funding};

/// What a test network's Robinhood driver waits before it stops trusting a
/// feed, and the window of each pool's time-weighted price: mainnet's.
const MAX_FEED_AGE: u64 = 4 * 86_400;
const TWAP_WINDOW: u32 = 30 * 60;

alloy::sol! {
    /// The mirror feed's constructor (`contracts/evm/test/testnet/MirrorFeed.sol`).
    contract MirrorFeed {
        constructor(
            address updater_,
            uint8 decimals_,
            string description_,
            uint256 version_,
            address source_,
            uint256 sourceChainId_,
            uint80 roundId,
            int256 answer,
            uint256 updatedAt
        );
    }

    /// The Robinhood driver's constructor
    /// (`contracts/evm/src/drivers/robinhood/RobinhoodDriver.sol`).
    contract RobinhoodDriver {
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

    /// Wrapped ether's deposit.
    interface IWrapped {
        function deposit() external payable;
    }

    /// Uniswap v3's position manager: a full-range position.
    interface IPositions {
        struct MintParams {
            address token0;
            address token1;
            uint24 fee;
            int24 tickLower;
            int24 tickUpper;
            uint256 amount0Desired;
            uint256 amount1Desired;
            uint256 amount0Min;
            uint256 amount1Min;
            address recipient;
            uint256 deadline;
        }
        function mint(MintParams calldata params)
            external
            payable
            returns (uint256 tokenId, uint128 liquidity, uint256 amount0, uint256 amount1);
    }
}

/// One step of the set-up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Step {
    Feeds,
    Gas,
    Inventory,
    Pools,
    Driver,
}

/// What the set-up does, and how much.
#[derive(Clone, Debug)]
pub struct Options {
    pub steps: Vec<Step>,
    /// The market account's gas, in the chain's coin, instead of the
    /// funding file's amount.
    pub gas: Option<f64>,
    pub stock_tokens: f64,
    pub weth: f64,
    pub usdc: f64,
    pub pool_tokens: f64,
    pub propose: bool,
    /// The market's Solana account's SOL, instead of the funding file's
    /// amount.
    pub sol: Option<f64>,
    /// The USDC the Solana seed opens with.
    pub orca_usdc: f64,
    /// Every endpoint must be a local fork (anvil): nothing reaches a
    /// public chain.
    pub on_fork: bool,
}

/// The driver a test network's deploy put on one chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriverRecord {
    pub chain_id: u64,
    pub address: Address,
}

impl DriverRecord {
    pub fn file_name(chain_id: u64) -> String {
        format!("robinhood-driver-evm-{chain_id}.json")
    }

    fn read(dir: &Path, chain_id: u64) -> Result<Option<Self>> {
        let path = dir.join(Self::file_name(chain_id));
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(
                serde_json::from_str(&text)
                    .with_context(|| format!("reading {}", path.display()))?,
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(anyhow!("reading {}: {error}", path.display())),
        }
    }

    fn write(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(Self::file_name(self.chain_id));
        std::fs::write(&path, serde_json::to_string_pretty(self)? + "\n")
            .with_context(|| format!("writing {}", path.display()))
    }
}

/// Everything a chain's steps share.
struct OnChain<'a> {
    key: &'a ChainKey,
    venue: &'static Venue,
    client: Client,
    funder: DynProvider,
    from: Address,
    market: Address,
    reserve: U256,
    dir: &'a Path,
    artifacts: &'a Path,
}

/// Run the steps on every chain of the network with a test venue.
pub async fn run(
    network: NetworkName,
    chains: Vec<(ChainKey, ChainConfig)>,
    configs: &ChainConfigs,
    deployments_dir: &Path,
    artifacts: &Path,
    funding_file: &Path,
    options: &Options,
) -> Result<()> {
    if network != NetworkName::Testnet {
        bail!(
            "testnet-market-setup sets the test networks' market up, and runs only with NETWORK=testnet"
        );
    }
    let funding = Funding::read(funding_file)?;
    let funder = fund::funder()?;
    let market = fund::market_account()?;
    let mut steps = options.steps.clone();
    steps.sort();
    steps.dedup();
    println!("testnet-market's account: {market:?}");
    let mut failed = Vec::new();
    for (key, config) in chains {
        if key.kind == ChainKind::Solana {
            println!("{} ({key})", config.label);
            if let Err(error) = solana(network, &key, &config, &funding, &steps, options).await {
                println!("  failed: {error:#}");
                failed.push(format!("{key}"));
            }
            continue;
        }
        let Some(venue) = key.evm_chain_id().and_then(listings::test_venue) else {
            continue;
        };
        let client = chain::connect_to(network, &key, &config).await?;
        if options.on_fork {
            let version = client
                .raw_request("web3_clientVersion", serde_json::json!([]))
                .await
                .context("asking the endpoint what it is")?
                .as_str()
                .unwrap_or_default()
                .to_string();
            if !version.to_ascii_lowercase().contains("anvil") {
                bail!(
                    "{} is not a local fork ({version}): --on-fork sends nothing to a public chain",
                    config.label
                );
            }
        }
        let on = OnChain {
            key: &key,
            venue,
            funder: client.signing_provider(funder.clone())?,
            client,
            from: funder.address(),
            market,
            reserve: funding.reserve(&key).unwrap_or(U256::ZERO),
            dir: deployments_dir,
            artifacts,
        };
        println!("{} ({key}), funder {:?}", config.label, on.from);
        for step in &steps {
            let outcome = match step {
                Step::Feeds => feeds(&on, configs).await,
                Step::Gas => gas(&on, &funding, options.gas).await,
                Step::Inventory => inventory(&on, options).await,
                Step::Pools => pools(&on, options.pool_tokens).await,
                Step::Driver => driver(&on, options.propose).await,
            };
            if let Err(error) = outcome {
                println!("  {step:?}: failed: {error:#}");
                failed.push(format!("{key} {step:?}"));
                break;
            }
        }
    }
    if !failed.is_empty() {
        bail!("testnet-market-setup failed: {}", failed.join(", "));
    }
    Ok(())
}

/// The market's steps on a Solana cluster (see the module's documentation).
async fn solana(
    network: NetworkName,
    key: &ChainKey,
    config: &ChainConfig,
    funding: &Funding,
    steps: &[Step],
    options: &Options,
) -> Result<()> {
    use chain::solana::Signer;
    use testnet_market::orca;
    let phrase = crate::solana::testnet_mnemonic()?;
    let market = crate::solana::keypair_from_mnemonic(&phrase, crate::devnet_keys::MARKET)?;
    let who = market.pubkey();
    let client = chain::connect_solana(network, key, config).await?;
    if options.on_fork {
        println!(
            "  skipped: --on-fork sends nothing to a public chain, and this cluster is not a fork"
        );
        return Ok(());
    }
    println!("  testnet-market's Solana account: {who}");
    let sol = |lamports: u64| format!("{:.4} SOL", lamports as f64 / 1e9);
    for step in steps {
        match step {
            Step::Feeds | Step::Driver => {
                println!(
                    "  {step:?}: nothing on Solana (Pyth's price is on the cluster; the Orca driver is set up by solana-setup)"
                );
            }
            Step::Gas => {
                let target = match options.sol {
                    Some(amount) => (amount.max(0.0) * 1e9) as u64,
                    None => funding.market_lamports(key)?.ok_or_else(|| {
                        anyhow!("the funding file has no market amount for {key}: pass --sol")
                    })?,
                };
                let held = client.lamports(&who).await?;
                let need = target.saturating_sub(held);
                if need == 0 {
                    println!("  gas: holds {}, enough", sol(held));
                    continue;
                }
                let deployer =
                    crate::solana::keypair_from_mnemonic(&phrase, crate::solana::DEPLOYER)?;
                let reserve = funding.solana(key)?.map_or(0, |amounts| amounts.reserve);
                let spare = client
                    .lamports(&deployer.pubkey())
                    .await?
                    .saturating_sub(reserve);
                if spare < need {
                    bail!(
                        "the deployer ({}) can give {} past its reserve, short of the {} the market needs: fund the deployer from devnet's faucet",
                        deployer.pubkey(),
                        sol(spare),
                        sol(need)
                    );
                }
                let landed = client
                    .send(
                        vec![chain::solana::executor::ix_fund(
                            &deployer.pubkey(),
                            &who,
                            need,
                        )],
                        &[&deployer],
                    )
                    .await
                    .context("sending the market its SOL")?;
                println!("  gas: sent {} ({})", sol(need), landed.record.signature);
            }
            Step::Inventory => {
                let usdc = client.token_balance(&orca::dollars_of(&who)).await?;
                let wrapped = client
                    .token_balance(&chain::solana::orca::wsol_of(&who))
                    .await?;
                println!(
                    "  inventory: {} USDC, {} wrapped and {} as coin",
                    usdc as f64 / 1e6,
                    sol(wrapped),
                    sol(client.lamports(&who).await?)
                );
                let wanted = (options.orca_usdc.max(0.0) * 1e6) as u64 + 5_000_000;
                if usdc < wanted {
                    println!(
                        "  inventory: short of {} USDC: tap Circle's faucet (Solana devnet, USDC) for {who}",
                        (wanted - usdc) as f64 / 1e6
                    );
                }
            }
            Step::Pools => {
                if client
                    .account(&chain::solana::orca::POOLS[0].address)
                    .await?
                    .is_none()
                {
                    let pyth = chain::solana::orca::setup::pyth_now(&client).await?;
                    chain::solana::orca::setup::ensure_pool(&client, &market, pyth).await?;
                    println!("  pools: created Orca's pool at Pyth's price");
                }
                let price = orca::walk_to_pyth(&client, &market, 25.0, 12, None).await?;
                println!("  pools: Orca's pool is at ${price:.2}, with Pyth's price");
                let dollars = (options.orca_usdc.max(0.0) * 1e6) as u64;
                orca::open_seed(&client, &market, dollars).await?;
                let seed = chain::solana::orca::setup::seed_position(&client, &market).await?;
                match seed {
                    Some(seed) => println!(
                        "  pools: the market's seed holds liquidity {} across ticks {}..{}",
                        seed.liquidity, seed.tick_lower, seed.tick_upper
                    ),
                    None => println!("  pools: no seed open"),
                }
            }
        }
    }
    Ok(())
}

/// Send `request` from the funder and wait for it to land; its hash and the
/// gas it used, said.
async fn send(
    on: &OnChain<'_>,
    request: TransactionRequest,
    what: &str,
) -> Result<TransactionReceipt> {
    let price = on.funder.get_gas_price().await?;
    let request = request
        .with_from(on.from)
        .with_gas_price(price + price / 5 + 1);
    let receipt = on
        .funder
        .send_transaction(request)
        .await
        .with_context(|| format!("sending {what}"))?
        .with_timeout(Some(Duration::from_secs(180)))
        .get_receipt()
        .await
        .with_context(|| format!("waiting for {what}"))?;
    let cost = U256::from(receipt.gas_used) * U256::from(receipt.effective_gas_price);
    println!(
        "    {what}: {:#x}, gas {} at {} gwei, {} ETH",
        receipt.transaction_hash,
        receipt.gas_used,
        format_units(U256::from(receipt.effective_gas_price), 9).unwrap_or_default(),
        format_ether(cost)
    );
    if !receipt.status() {
        bail!("{what} reverted ({:#x})", receipt.transaction_hash);
    }
    Ok(receipt)
}

async fn call<C: SolCall>(
    on: &OnChain<'_>,
    to: Address,
    call: C,
    what: &str,
) -> Result<TransactionReceipt> {
    send(
        on,
        TransactionRequest::default()
            .with_to(to)
            .with_input(Bytes::from(call.abi_encode())),
        what,
    )
    .await
}

/// The mainnet chain the mirrors copy, read with no key.
fn mainnet(configs: &ChainConfigs) -> Result<DynProvider> {
    let key = ChainKey::new(ChainKind::Evm, listings::MAINNET.to_string());
    let config = configs
        .get(&key)
        .ok_or_else(|| anyhow!("the chain config has no {key}"))?;
    Ok(ProviderBuilder::new()
        .connect_http(
            config
                .first_rpc()
                .parse()
                .map_err(|error| anyhow!("{key}'s RPC URL: {error}"))?,
        )
        .erased())
}

async fn feeds(on: &OnChain<'_>, configs: &ChainConfigs) -> Result<()> {
    let chain_id = on.venue.chain;
    let mut record = MirrorFeeds::read(on.dir, chain_id)?;
    let mirrored: Vec<_> = on
        .venue
        .listed
        .iter()
        .filter_map(|listing| match listing.feed {
            Feed::Mirror { chain, source } => Some((listing.token.symbol, chain, source)),
            Feed::Chainlink(_) => None,
        })
        .collect();
    if mirrored.is_empty() {
        println!("  feeds: none to mirror (the chain has its own)");
        return Ok(());
    }
    let source_chain = mainnet(configs)?;
    for (symbol, chain, source) in mirrored {
        if chain != listings::MAINNET {
            bail!("{symbol} mirrors chain {chain}, which this set-up doesn't read");
        }
        if let Some(existing) = record.feeds.get(symbol)
            && existing.source == source
            && existing.source_chain_id == chain
            && on.client.has_code(existing.address).await?
        {
            let updater = IMirrorFeed::new(existing.address, on.client.provider())
                .updater()
                .call()
                .await?;
            if updater != on.market {
                bail!(
                    "{symbol}'s mirror {:?} takes rounds from {updater:?}, not the market's account",
                    existing.address
                );
            }
            println!("  feeds: {symbol} at {:?}, already there", existing.address);
            continue;
        }
        let feed = AggregatorV3Interface::new(source, &source_chain);
        let latest = feed.latestRoundData().call().await?;
        let decimals = feed.decimals().call().await?;
        if decimals != FEED_DECIMALS {
            bail!("{symbol}'s mainnet feed has {decimals} decimals");
        }
        let description = feed.description().call().await?;
        let version = feed.version().call().await?;
        let mut code = creation_code(on.artifacts, "MirrorFeed", &[])?;
        code.extend(
            MirrorFeed::constructorCall {
                updater_: on.market,
                decimals_: decimals,
                description_: description,
                version_: version,
                source_: source,
                sourceChainId_: U256::from(chain),
                roundId: latest.roundId,
                answer: latest.answer,
                updatedAt: latest.updatedAt,
            }
            .abi_encode(),
        );
        let receipt = send(
            on,
            TransactionRequest::default().with_deploy_code(Bytes::from(code)),
            &format!("{symbol}'s mirror feed"),
        )
        .await?;
        let address = receipt
            .contract_address
            .ok_or_else(|| anyhow!("the chain gave the mirror no address"))?;
        record.feeds.insert(
            symbol.to_string(),
            MirrorRecord {
                address,
                source,
                source_chain_id: chain,
            },
        );
        // Written after each one, so a set-up stopped halfway keeps what it
        // made.
        record.chain_id = chain_id;
        let path = record.write(on.dir)?;
        println!(
            "  feeds: {symbol} at {address:?}, recorded in {}",
            path.display()
        );
    }
    Ok(())
}

async fn gas(on: &OnChain<'_>, funding: &Funding, gas: Option<f64>) -> Result<()> {
    let target = match gas {
        Some(amount) => {
            parse_ether(&format!("{amount}")).map_err(|error| anyhow!("--gas {amount}: {error}"))?
        }
        None => funding
            .market_target(on.key)
            .ok_or_else(|| anyhow!("the funding file names no market amount for {}", on.key))?,
    };
    let balance = on.funder.get_balance(on.market).await?;
    let need = target.saturating_sub(balance);
    if need.is_zero() {
        println!("  gas: the market holds {}, enough", format_ether(balance));
        return Ok(());
    }
    let available = on
        .funder
        .get_balance(on.from)
        .await?
        .saturating_sub(on.reserve);
    if need > available {
        bail!(
            "the market needs {} and the funder has {} past its reserve",
            format_ether(need),
            format_ether(available)
        );
    }
    let hash = fund::send_value(&on.funder, on.from, on.market, need).await?;
    println!(
        "  gas: sent the market {} ({hash}); it holds {}",
        format_ether(need),
        format_ether(target)
    );
    Ok(())
}

fn units(amount: f64, decimals: u8) -> Result<U256> {
    let parsed = parse_units(&format!("{amount}"), decimals)
        .map_err(|error| anyhow!("{amount}: {error}"))?;
    if parsed.is_negative() {
        bail!("{amount} is not an amount");
    }
    Ok(parsed.get_absolute())
}

async fn inventory(on: &OnChain<'_>, options: &Options) -> Result<()> {
    let provider = on.client.provider();
    for listing in on.venue.listed {
        let token = listing.token;
        let wrapped = token.symbol == "WETH";
        let wanted = units(
            if wrapped {
                options.weth
            } else {
                options.stock_tokens
            },
            token.decimals,
        )?;
        let erc20 = IToken::new(token.address, provider);
        let held = erc20.balanceOf(on.market).call().await?;
        let need = wanted.saturating_sub(held);
        if need.is_zero() {
            println!(
                "  inventory: the market holds {} {}, enough",
                format_units(held, token.decimals)?,
                token.symbol
            );
            continue;
        }
        let funder_holds = erc20.balanceOf(on.from).call().await?;
        // The funder's balance once it has wrapped, read at the wrap's own
        // block: a public endpoint may answer "latest" from a node a block
        // behind, which doesn't yet show the wrap.
        let mut wrapped_at = None;
        if wrapped && funder_holds < need {
            let wrap = need - funder_holds;
            let coin = on.funder.get_balance(on.from).await?;
            if coin.saturating_sub(on.reserve) < wrap {
                bail!(
                    "the funder can't wrap {} past its reserve",
                    format_ether(wrap)
                );
            }
            let receipt = send(
                on,
                TransactionRequest::default()
                    .with_to(token.address)
                    .with_value(wrap)
                    .with_input(Bytes::from(IWrapped::depositCall {}.abi_encode())),
                "wrapping the funder's coin",
            )
            .await?;
            wrapped_at = receipt.block_number;
        } else if funder_holds < need {
            println!(
                "  inventory: the market needs {} {} and the funder holds {}: sending what it has",
                format_units(need, token.decimals)?,
                token.symbol,
                format_units(funder_holds, token.decimals)?
            );
        }
        let funder_holds = match wrapped_at {
            Some(block) => balance_at(&erc20, on.from, block).await?,
            None => funder_holds,
        };
        let sent = need.min(funder_holds);
        if sent.is_zero() {
            continue;
        }
        call(
            on,
            token.address,
            IToken::transferCall {
                to: on.market,
                amount: sent,
            },
            &format!(
                "{} {} to the market",
                format_units(sent, token.decimals)?,
                token.symbol
            ),
        )
        .await?;
    }
    // The dollar, where it is finite.
    if on.venue.chain != listings::TESTNET {
        let dollar = on.venue.dollar;
        let wanted = units(options.usdc, dollar.decimals)?;
        let erc20 = IToken::new(dollar.address, provider);
        let held = erc20.balanceOf(on.market).call().await?;
        let need = wanted
            .saturating_sub(held)
            .min(erc20.balanceOf(on.from).call().await?);
        if need.is_zero() {
            println!(
                "  inventory: the market holds {} {}",
                format_units(held, dollar.decimals)?,
                dollar.symbol
            );
        } else {
            call(
                on,
                dollar.address,
                IToken::transferCall {
                    to: on.market,
                    amount: need,
                },
                &format!(
                    "{} {} to the market",
                    format_units(need, dollar.decimals)?,
                    dollar.symbol
                ),
            )
            .await?;
        }
    }
    Ok(())
}

/// `who`'s balance of `token` as of `block`, waiting for the endpoint to
/// reach that block if it hasn't yet.
async fn balance_at<P: Provider>(
    token: &IToken::ITokenInstance<P>,
    who: Address,
    block: u64,
) -> Result<U256> {
    let mut tries = 0;
    loop {
        match token
            .balanceOf(who)
            .block(alloy::eips::BlockId::number(block))
            .call()
            .await
        {
            Ok(balance) => return Ok(balance),
            Err(_) if tries < 10 => {
                tries += 1;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("reading the balance at block {block}"));
            }
        }
    }
}

/// The lowest and highest ticks a pool at `spacing` can hold a position
/// between.
fn full_range(spacing: i32) -> (i32, i32) {
    let top = 887_272 / spacing * spacing;
    (-top, top)
}

async fn pools(on: &OnChain<'_>, pool_tokens: f64) -> Result<()> {
    if pool_tokens <= 0.0 {
        println!("  pools: nothing to add (--pool-tokens)");
        return Ok(());
    }
    let provider = on.client.provider();
    let positions = on.venue.uniswap.positions;
    let mintable = on.venue.chain == listings::TESTNET;
    for listing in on.venue.listed {
        let token = listing.token;
        let dollar = on.venue.dollar;
        let pool_address = listings::pool_address(
            on.venue.uniswap.factory,
            token.address,
            dollar.address,
            listing.fee,
        );
        let pool = IUniswapV3Pool::new(pool_address, provider);
        let token0 = pool.token0().call().await?;
        let slot0 = pool.slot0().call().await?;
        let spacing = pool.tickSpacing().call().await?.as_i32();
        let token_is_token0 = token0 == token.address;
        let amount = units(pool_tokens, token.decimals)?;
        // At the pool's price, a full-range position takes the dollar in
        // the ratio token1 per token0 = (sqrtPrice / 2^96)^2.
        let sqrt = U256::from(slot0.sqrtPriceX96);
        let q192 = U256::from(1u8) << 192usize;
        let dollars = if token_is_token0 {
            testnet_market::math::mul_div_up(amount, sqrt * sqrt, q192)
        } else {
            testnet_market::math::mul_div_up(amount, q192, sqrt * sqrt)
        }
        .ok_or_else(|| anyhow!("{}'s pool price is out of range", token.symbol))?;
        // A little over, so the token side is the one used up.
        let dollars = dollars + dollars / U256::from(100u8) + U256::from(1u8);
        let token_erc20 = IToken::new(token.address, provider);
        if token_erc20.balanceOf(on.from).call().await? < amount {
            bail!("the funder holds less than {pool_tokens} {}", token.symbol);
        }
        let dollar_erc20 = IToken::new(dollar.address, provider);
        let have = dollar_erc20.balanceOf(on.from).call().await?;
        if have < dollars {
            if !mintable {
                bail!(
                    "the funder holds {} {}, short of {}",
                    format_units(have, dollar.decimals)?,
                    dollar.symbol,
                    format_units(dollars, dollar.decimals)?
                );
            }
            call(
                on,
                dollar.address,
                IToken::mintCall {
                    to: on.from,
                    amount: dollars - have,
                },
                &format!(
                    "minting {} {}",
                    format_units(dollars - have, dollar.decimals)?,
                    dollar.symbol
                ),
            )
            .await?;
        }
        for (asset, needed) in [(token.address, amount), (dollar.address, dollars)] {
            let erc20 = IToken::new(asset, provider);
            if erc20.allowance(on.from, positions).call().await? < needed {
                call(
                    on,
                    asset,
                    IToken::approveCall {
                        spender: positions,
                        amount: U256::MAX,
                    },
                    "approving the position manager",
                )
                .await?;
            }
        }
        let (lower, upper) = full_range(spacing);
        let (token0, token1, amount0, amount1) = if token_is_token0 {
            (token.address, dollar.address, amount, dollars)
        } else {
            (dollar.address, token.address, dollars, amount)
        };
        let deadline = U256::from(on.client.block_timestamp().await? + 1_800);
        call(
            on,
            positions,
            IPositions::mintCall {
                params: IPositions::MintParams {
                    token0,
                    token1,
                    fee: U24::from(listing.fee),
                    tickLower: I24::try_from(lower).map_err(|_| anyhow!("tick {lower}"))?,
                    tickUpper: I24::try_from(upper).map_err(|_| anyhow!("tick {upper}"))?,
                    amount0Desired: amount0,
                    amount1Desired: amount1,
                    amount0Min: U256::ZERO,
                    amount1Min: U256::ZERO,
                    recipient: on.from,
                    deadline,
                },
            },
            &format!("{pool_tokens} {} into its pool", token.symbol),
        )
        .await?;
    }
    Ok(())
}

/// Refuse a driver for a deployment whose dollar (what its stations take
/// deposits in) is not the dollar the venue trades against: nothing on the
/// chain ties the two, and a station whose driver trades another dollar
/// counts that dollar as its own.
fn same_dollar(deployment: Address, venue: Address) -> Result<()> {
    if deployment != venue {
        bail!(
            "the deployment's dollar is {deployment:?} and the venue trades against {venue:?}: deploy the platform again with the venue's dollar before its driver"
        );
    }
    Ok(())
}

async fn driver(on: &OnChain<'_>, propose: bool) -> Result<()> {
    let chain_id = on.venue.chain;
    let mirrors = MirrorFeeds::read(on.dir, chain_id)?;
    let resolved = on.venue.resolve(&mirrors)?;
    let admin = on.client.account(Role::Admin)?;
    let deployments = on
        .client
        .deployments(&chain::Deployments::file_in(on.dir, on.key))
        .await?;
    same_dollar(deployments.usd, on.venue.dollar.address)?;
    let existing = DriverRecord::read(on.dir, chain_id)?;
    let address = match existing {
        Some(record) if on.client.has_code(record.address).await? => {
            println!("  driver: at {:?}, already there", record.address);
            record.address
        }
        _ => {
            let mut code = creation_code(on.artifacts, "RobinhoodDriver", &[])?;
            code.extend(
                RobinhoodDriver::constructorCall {
                    venue: RobinhoodDriver::DriverVenue {
                        dollar: on.venue.dollar.address,
                        router: on.venue.uniswap.router,
                        safeSingleton: SAFE_SINGLETON,
                        safeFactory: SAFE_PROXY_FACTORY,
                        rolesMastercopy: ROLES_MASTERCOPY,
                        moduleFactory: MODULE_PROXY_FACTORY,
                        maxFeedAge: U256::from(MAX_FEED_AGE),
                        twapWindow: TWAP_WINDOW,
                    },
                    listings: driver_listings(&resolved)?,
                }
                .abi_encode(),
            );
            let address = on
                .client
                .deploy(admin, code)
                .await
                .context("deploying the Robinhood driver")?;
            DriverRecord { chain_id, address }.write(on.dir)?;
            println!(
                "  driver: deployed at {address:?}, recorded in {}",
                on.dir.join(DriverRecord::file_name(chain_id)).display()
            );
            address
        }
    };
    // Under the name `config/drivers.toml` lists it by on this chain:
    // `robinhood` on Robinhood Chain, `uniswap-base` on Base.
    let listed_as = on.venue.driver;
    let name = market_id(listed_as);
    let trading = on
        .client
        .read(
            deployments.attestation,
            IAttestationRegistry::tradingDriverCall { name },
        )
        .await?;
    if trading == address {
        println!("  driver: the registry trades it as {listed_as}");
        return Ok(());
    }
    if !propose {
        println!(
            "  driver: not proposed (the registry trades {trading:?} as {listed_as}); --propose proposes it"
        );
        return Ok(());
    }
    let receipt = on
        .client
        .send(
            admin,
            deployments.attestation,
            IAttestationRegistry::proposeDriverCall {
                name,
                implementation: address,
            },
        )
        .await
        .context("proposing the Robinhood driver")?;
    println!(
        "  driver: proposed as {listed_as} ({:#x}, gas {})",
        receipt.transaction_hash, receipt.gas_used
    );
    Ok(())
}

/// What the driver's constructor lists: each resolved token with a maximum
/// sale, the rest kept priced but not listed. More than the driver takes,
/// or none, is refused before anything is sent.
fn driver_listings(resolved: &[Resolved]) -> Result<Vec<RobinhoodDriver::DriverListing>> {
    let listings: Vec<_> = resolved
        .iter()
        .filter_map(|listing| {
            listing
                .max_sale
                .map(|max_sale| RobinhoodDriver::DriverListing {
                    token: listing.token.address,
                    feed: listing.feed,
                    pool: listing.pool,
                    maxSale: U256::from(max_sale),
                })
        })
        .collect();
    if listings.is_empty() || listings.len() > MAX_LISTINGS {
        bail!(
            "the driver would list {} tokens, and it takes 1 to {MAX_LISTINGS}",
            listings.len()
        );
    }
    if listings.iter().any(|listing| listing.maxSale.is_zero()) {
        bail!("a listing's max sale is zero, and the driver refuses zero");
    }
    Ok(listings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(venue: &Venue) -> Vec<Resolved> {
        let mirrors = MirrorFeeds {
            chain_id: venue.chain,
            feeds: venue
                .listed
                .iter()
                .filter_map(|listing| match listing.feed {
                    Feed::Mirror { chain, source } => Some((
                        listing.token.symbol.to_string(),
                        MirrorRecord {
                            address: Address::repeat_byte(0xfe),
                            source,
                            source_chain_id: chain,
                        },
                    )),
                    Feed::Chainlink(_) => None,
                })
                .collect(),
        };
        venue.resolve(&mirrors).unwrap()
    }

    #[test]
    fn each_test_venue_s_driver_is_built_from_at_most_its_maximum_listings_with_their_max_sales() {
        for venue in [&listings::ROBINHOOD_TESTNET, &listings::BASE_SEPOLIA_VENUE] {
            let all = resolved(venue);
            let built = driver_listings(&all).unwrap();
            assert!(built.len() <= MAX_LISTINGS, "{}", venue.label);
            for listing in &built {
                let source = all
                    .iter()
                    .find(|resolved| resolved.token.address == listing.token)
                    .unwrap();
                assert_eq!(listing.maxSale, U256::from(source.max_sale.unwrap()));
                assert_eq!((listing.feed, listing.pool), (source.feed, source.pool));
            }
        }
    }

    #[test]
    fn a_driver_listing_more_than_it_takes_or_nothing_is_refused() {
        let mut all = resolved(&listings::ROBINHOOD_TESTNET);
        for listing in &mut all {
            listing.max_sale = Some(1);
        }
        assert!(driver_listings(&all).is_err());
        for listing in &mut all {
            listing.max_sale = None;
        }
        assert!(driver_listings(&all).is_err());
        all[0].max_sale = Some(0);
        assert!(driver_listings(&all).is_err());
    }

    #[test]
    fn a_driver_is_refused_for_a_deployment_whose_dollar_is_not_the_venue_s() {
        let venue = Address::repeat_byte(0x0d);
        assert!(same_dollar(venue, venue).is_ok());
        assert!(same_dollar(Address::repeat_byte(0x1e), venue).is_err());
    }
}
