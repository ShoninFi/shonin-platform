//! `testnet-market`: the service. See the library's documentation for what
//! it does; this reads its settings, finds its key, and runs the rounds.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, anyhow, bail};
use chain::{ChainConfigs, ChainKey, ChainKind, NetworkName};
use clap::Parser;
use robinhood::listings::{self, MirrorFeeds};
use testnet_market::bindings::IMirrorFeed;
use testnet_market::health::{self, Health};
use testnet_market::homes::Homes;
use testnet_market::market::{ChainMarket, Endpoints, Market, Settings};
use testnet_market::orca::SolanaMarket;
use testnet_market::{
    KEY_ENV, MARKET_INDEX, MNEMONIC_ENV, SOLANA_KEY_ENV, SOLANA_MARKET_INDEX, SOLANA_MNEMONIC_ENV,
};

#[derive(Parser)]
#[command(about = "Keeps the test networks' pools at the real market price")]
struct Args {
    /// Which chains: only `testnet` is served.
    #[arg(long, env = "SHONIN_NETWORK")]
    network: NetworkName,
    /// Every chain the platform knows, and what each network means.
    #[arg(long, env = "SHONIN_CHAINS", default_value = "config/chains.toml")]
    chains: PathBuf,
    /// Where the network's deployment files are, the mirror feeds' records
    /// among them.
    #[arg(
        long,
        env = "SHONIN_DEPLOYMENTS_DIR",
        default_value = ".shonin/testnet/deployments"
    )]
    deployments_dir: PathBuf,
    /// Keep only this chain (as `evm:<id>` or `solana:devnet`), instead of
    /// every one the network has a venue on.
    #[arg(long)]
    only: Option<ChainKey>,
    /// Where what each finite asset is bought back to is kept, so a restart
    /// keeps it; none keeps it in memory alone.
    #[arg(
        long,
        env = "SHONIN_TESTNET_MARKET_HOMES",
        default_value = ".shonin/testnet/testnet-market-homes.json"
    )]
    homes: Option<PathBuf>,
    /// Seconds between rounds.
    #[arg(long, default_value_t = 60)]
    every: u64,
    /// Outside this many basis points from its feed, a pool is taken back.
    #[arg(long, default_value_t = Settings::default().band_bps)]
    band_bps: f64,
    /// The most one trade spends, in dollars at the feed's price.
    #[arg(long, default_value_t = Settings::default().max_trade_usd)]
    max_trade_usd: f64,
    /// Read and decide, and send nothing.
    #[arg(long)]
    dry_run: bool,
    /// One round, then exit.
    #[arg(long)]
    once: bool,
    /// The port `/health` answers on; 0 for none.
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// The address the health route listens on: 0.0.0.0 in a container.
    #[arg(long, default_value = "127.0.0.1")]
    listen: std::net::IpAddr,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let args = Args::parse();
    if args.network != NetworkName::Testnet {
        bail!(
            "testnet-market keeps the test networks' pools alone: run it with NETWORK=testnet, never {} (it holds a test network key, and mainnet's pools are the market's own)",
            args.network
        );
    }
    let signer = key()?;
    let account = signer.address();
    let configs = ChainConfigs::load(&args.chains)?;

    // The feeds are read on mainnet, with no key.
    let source_key = ChainKey::new(ChainKind::Evm, listings::MAINNET.to_string());
    let source_config = configs
        .get(&source_key)
        .ok_or_else(|| anyhow!("the chain config has no {source_key}"))?;
    let source = ProviderBuilder::new()
        .connect_http(
            source_config
                .first_rpc()
                .parse()
                .map_err(|error| anyhow!("{source_key}'s RPC URL: {error}"))?,
        )
        .erased();
    let source_chain = source.get_chain_id().await.context("reading mainnet")?;
    if source_chain != listings::MAINNET {
        bail!("{source_key}'s endpoint is chain {source_chain}");
    }

    let settings = Settings {
        band_bps: args.band_bps,
        max_trade_usd: args.max_trade_usd,
        dry_run: args.dry_run,
        ..Settings::default()
    };
    let health = Arc::new(Mutex::new(Health {
        account: account.to_string(),
        every: args.every,
        ..Health::default()
    }));

    let network = configs
        .network(NetworkName::Testnet)
        .ok_or_else(|| anyhow!("the chain config has no testnet"))?;
    let mut chains = Vec::new();
    for (_, key) in network.iter() {
        if args.only.as_ref().is_some_and(|only| !only.same_chain(key)) {
            continue;
        }
        let Some(venue) = key.evm_chain_id().and_then(listings::test_venue) else {
            continue;
        };
        let config = configs
            .get(key)
            .ok_or_else(|| anyhow!("the chain config has no {key}"))?;
        let endpoints = Endpoints::new(&config.rpc, signer.clone())?;
        let chain_id = endpoints
            .provider()
            .get_chain_id()
            .await
            .with_context(|| format!("reading {key}"))?;
        if chain_id != venue.chain {
            bail!("{key}'s endpoint is chain {chain_id}");
        }
        let mirrors = MirrorFeeds::read(&args.deployments_dir, venue.chain)?;
        // Anyone may mint the Robinhood testnet's USDG; Base Sepolia's USDC
        // comes from Circle's faucet alone.
        let mintable = venue.chain == listings::TESTNET;
        let market = match ChainMarket::new(key.to_string(), endpoints, venue, &mirrors, mintable) {
            Ok(market) => market,
            Err(error) => {
                tracing::warn!(chain = %key, error = format!("{error:#}"), "this chain is not kept");
                if let Ok(mut health) = health.lock() {
                    let chain = health.chain(&key.to_string());
                    chain.label = venue.label.to_string();
                    chain.error = Some(format!("{error:#}"));
                }
                continue;
            }
        };
        for mirror in &market.mirrors {
            let updater = IMirrorFeed::new(mirror.mirror, market.endpoints.provider())
                .updater()
                .call()
                .await
                .with_context(|| format!("reading {}'s mirror feed", mirror.symbol))?;
            if updater != account {
                bail!(
                    "{}'s mirror feed on {key} takes rounds from {updater}, not this account ({account})",
                    mirror.symbol
                );
            }
        }
        tracing::info!(chain = %key, label = venue.label, pools = market.pools.len(), mirrors = market.mirrors.len(), "keeping");
        chains.push(market);
    }
    // Solana devnet's Orca pool, with the Solana account's own key.
    let mut solana = None;
    for (_, key) in network.iter() {
        if key.kind != ChainKind::Solana
            || args.only.as_ref().is_some_and(|only| !only.same_chain(key))
        {
            continue;
        }
        let Some(signer) = solana_key()? else {
            tracing::warn!(chain = %key, "no Solana key ({SOLANA_KEY_ENV} or {SOLANA_MNEMONIC_ENV}): this chain is not kept");
            continue;
        };
        let config = configs
            .get(key)
            .ok_or_else(|| anyhow!("the chain config has no {key}"))?;
        let kept = async {
            let client = chain::solana::SolanaClient::connect_with(key, config, None).await?;
            SolanaMarket::new(key.to_string(), config.label.clone(), client, signer).await
        }
        .await;
        match kept {
            Ok(market) => {
                tracing::info!(chain = %key, account = %market.account(), pool = %chain::solana::orca::POOLS[0].address, "keeping");
                if let Ok(mut health) = health.lock() {
                    health.solana_account = Some(market.account().to_string());
                }
                solana = Some(market);
            }
            Err(error) => {
                tracing::warn!(chain = %key, error = format!("{error:#}"), "this chain is not kept");
                if let Ok(mut health) = health.lock() {
                    let chain = health.chain(&key.to_string());
                    chain.label = config.label.clone();
                    chain.error = Some(format!("{error:#}"));
                }
            }
        }
    }
    if chains.is_empty() && solana.is_none() {
        bail!("no chain to keep");
    }
    let homes = Homes::load(args.homes.clone())?;

    if args.port != 0 {
        let address = SocketAddr::from((args.listen, args.port));
        let health = health.clone();
        tokio::spawn(async move {
            if let Err(error) = health::serve(address, health).await {
                tracing::error!(error = format!("{error:#}"), "the health route stopped");
            }
        });
    }

    tracing::info!(%account, every = args.every, dry_run = args.dry_run, "testnet-market is up");
    let mut market = Market {
        account,
        source,
        chains,
        solana,
        settings,
        health,
        homes,
    };
    let mut ticker = tokio::time::interval(Duration::from_secs(args.every.max(1)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        market.round().await;
        if args.once {
            return Ok(());
        }
    }
}

/// The Solana account's keypair: handed over whole (in the cloud), or
/// derived from the test networks' Solana mnemonic at its own index (on a
/// developer's machine); none if neither is there.
fn solana_key() -> Result<Option<chain::solana::Keypair>> {
    let set = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    };
    if let Some(text) = set(SOLANA_KEY_ENV) {
        return chain::solana::keys::keypair_from_text(&text)
            .map(Some)
            .map_err(|error| anyhow!("{SOLANA_KEY_ENV}: {error}"));
    }
    match set(SOLANA_MNEMONIC_ENV) {
        Some(phrase) => {
            chain::solana::keys::keypair_from_mnemonic(&phrase, SOLANA_MARKET_INDEX).map(Some)
        }
        None => Ok(None),
    }
}

/// The account's key: handed over whole (in the cloud), or derived from the
/// test networks' mnemonic at its own index (on a developer's machine).
fn key() -> Result<PrivateKeySigner> {
    if let Some(key) = std::env::var(KEY_ENV)
        .ok()
        .filter(|key| !key.trim().is_empty())
    {
        return key
            .trim()
            .parse()
            .map_err(|_| anyhow!("{KEY_ENV} is not a private key"));
    }
    use alloy::signers::local::MnemonicBuilder;
    use alloy::signers::local::coins_bip39::English;
    let phrase = std::env::var(MNEMONIC_ENV)
        .ok()
        .filter(|phrase| !phrase.trim().is_empty())
        .ok_or_else(|| anyhow!("no key: set {KEY_ENV}, or {MNEMONIC_ENV} to derive it"))?;
    MnemonicBuilder::<English>::default()
        .phrase(phrase.trim())
        .index(MARKET_INDEX)
        .ok()
        .and_then(|builder| builder.build().ok())
        .ok_or_else(|| anyhow!("the EVM mnemonic is not a valid BIP-39 phrase"))
}
