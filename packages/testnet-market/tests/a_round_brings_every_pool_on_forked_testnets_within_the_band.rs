//! One round of the market against local forks of the test networks: the
//! real Uniswap v3 pools, tokens and (on Base Sepolia) Chainlink feed, with
//! mirror feeds deployed on the fork and the mainnet feeds read live. The
//! round posts each mainnet round the mirrors lack, then brings every pool
//! within the band of its feed; the round after finds nothing to post and
//! nothing to trade.
//!
//! Runs only with `ROBINHOOD_TESTNET_FORK_URL` (a Robinhood Chain testnet
//! RPC) and `ROBINHOOD_FORK_URL` (a Robinhood Chain mainnet RPC) set, and
//! Base Sepolia's part with `BASE_SEPOLIA_FORK_URL` as well; it needs
//! `anvil` and the contracts built (`forge build`). Nothing is sent to any
//! public chain: every send goes to the local forks.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::local::coins_bip39::English;
use alloy::sol_types::{SolConstructor, SolValue};
use robinhood::listings::{
    BASE_SEPOLIA_VENUE, FEED_DECIMALS, Feed, MirrorFeeds, MirrorRecord, ROBINHOOD_TESTNET, Venue,
};
use testnet_market::bindings::{AggregatorV3Interface, IToken, IUniswapV3Pool};
use testnet_market::health::Health;
use testnet_market::market::{ChainMarket, Endpoints, Market, Settings};
use testnet_market::math::{self, Pair};

alloy::sol! {
    /// The mirror feed's constructor, as the test deploys it.
    contract MirrorFeedConstructor {
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
}

struct Anvil(Child);

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if !(19000..=19200).contains(&port) {
            return port;
        }
    }
}

fn fork(url: &str) -> (Anvil, String) {
    let port = free_port();
    let child = Command::new("anvil")
        .args(["--port", &port.to_string(), "--fork-url", url, "--silent"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("anvil on the PATH");
    let anvil = Anvil(child);
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the fork never answered"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    (anvil, format!("http://127.0.0.1:{port}"))
}

/// Anvil's first development account (its published mnemonic).
fn account() -> PrivateKeySigner {
    alloy::signers::local::MnemonicBuilder::<English>::default()
        .phrase("test test test test test test test test test test test junk")
        .index(0)
        .unwrap()
        .build()
        .unwrap()
}

fn provider(url: &str, signer: &PrivateKeySigner) -> DynProvider {
    ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer.clone()))
        .connect_http(url.parse().unwrap())
        .erased()
}

fn mirror_creation_code() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/evm/out/MirrorFeed.sol/MirrorFeed.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{} missing: run forge build", path.display()));
    let artifact: serde_json::Value = serde_json::from_str(&text).unwrap();
    alloy::hex::decode(artifact["bytecode"]["object"].as_str().unwrap()).unwrap()
}

/// Set `holder`'s balance of `token` on the fork by finding the slot its
/// balances mapping keeps it in (a plain mapping at a low slot, or
/// OpenZeppelin's namespaced ERC-20 storage), as Foundry's `deal` does.
async fn deal(provider: &DynProvider, token: Address, holder: Address, amount: U256) {
    let erc20 = IToken::new(token, provider);
    let namespaced: B256 = "0x52c63247e1f47db19d5ce0460030c497f067ca4cebf71ba98eeadabe20bace00"
        .parse()
        .unwrap();
    let mut bases: Vec<U256> = (0u64..64).map(U256::from).collect();
    bases.push(namespaced.into());
    for base in bases {
        let slot = keccak256((holder, base).abi_encode());
        let before = provider.get_storage_at(token, slot.into()).await.unwrap();
        let probe = U256::from(0x5eed_u64);
        set_storage(provider, token, slot, probe).await;
        if erc20.balanceOf(holder).call().await.unwrap() == probe {
            set_storage(provider, token, slot, amount).await;
            assert_eq!(erc20.balanceOf(holder).call().await.unwrap(), amount);
            return;
        }
        set_storage(provider, token, slot, before).await;
    }
    panic!("no balance slot found for {token}");
}

async fn set_storage(provider: &DynProvider, at: Address, slot: B256, value: U256) {
    let _: bool = provider
        .raw_request("anvil_setStorageAt".into(), (at, slot, B256::from(value)))
        .await
        .unwrap();
}

/// Deploy a mirror of `source` updated by `updater`, starting from the
/// source's round before its latest, so the market has one to post.
async fn deploy_mirror(
    provider: &DynProvider,
    mainnet: &DynProvider,
    symbol: &str,
    source: Address,
    updater: Address,
) -> Address {
    let feed = AggregatorV3Interface::new(source, mainnet);
    let latest = feed.latestRoundData().call().await.unwrap();
    let earlier = feed
        .getRoundData(latest.roundId - alloy::primitives::aliases::U80::from(1u8))
        .call()
        .await
        .unwrap();
    let version = feed.version().call().await.unwrap();
    let args = MirrorFeedConstructor::constructorCall {
        updater_: updater,
        decimals_: FEED_DECIMALS,
        description_: format!("{symbol} / USD"),
        version_: version,
        source_: source,
        sourceChainId_: U256::from(robinhood::listings::MAINNET),
        roundId: earlier.roundId_,
        answer: earlier.answer,
        updatedAt: earlier.updatedAt,
    }
    .abi_encode();
    let mut code = mirror_creation_code();
    code.extend(args);
    let receipt = provider
        .send_transaction(TransactionRequest::default().with_deploy_code(Bytes::from(code)))
        .await
        .unwrap()
        .get_receipt()
        .await
        .unwrap();
    assert!(receipt.status());
    receipt.contract_address.unwrap()
}

/// The pool's premium over its feed, in basis points.
async fn premium(
    provider: &DynProvider,
    venue: &Venue,
    feed: Address,
    pool: Address,
    token: Address,
    decimals: u8,
) -> f64 {
    let answer = AggregatorV3Interface::new(feed, provider)
        .latestRoundData()
        .call()
        .await
        .unwrap()
        .answer
        .into_raw();
    let pool = IUniswapV3Pool::new(pool, provider);
    let token0 = pool.token0().call().await.unwrap();
    let pair = Pair {
        token_is_token0: token0 == token,
        token_decimals: decimals,
        dollar_decimals: venue.dollar.decimals,
        feed_decimals: FEED_DECIMALS,
    };
    let slot0 = pool.slot0().call().await.unwrap();
    math::token_premium_bps(
        &pair,
        U256::from(slot0.sqrtPriceX96),
        pair.sqrt_price_at(answer).unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_round_brings_every_pool_on_forked_testnets_within_the_band() {
    let (Ok(testnet_url), Ok(mainnet_url)) = (
        std::env::var("ROBINHOOD_TESTNET_FORK_URL"),
        std::env::var("ROBINHOOD_FORK_URL"),
    ) else {
        eprintln!("skipped: set ROBINHOOD_TESTNET_FORK_URL and ROBINHOOD_FORK_URL to run it");
        return;
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let base_url = std::env::var("BASE_SEPOLIA_FORK_URL").ok();
    let signer = account();
    let me = signer.address();
    let mainnet = ProviderBuilder::new()
        .connect_http(mainnet_url.parse().unwrap())
        .erased();

    // Robinhood Chain testnet: a mirror per stock, the account holding ten
    // of each stock token (dollars it mints).
    let (_robinhood_fork, robinhood_url) = fork(&testnet_url);
    let robinhood = provider(&robinhood_url, &signer);
    let mut mirrors = MirrorFeeds {
        chain_id: ROBINHOOD_TESTNET.chain,
        feeds: BTreeMap::new(),
    };
    for listing in ROBINHOOD_TESTNET.listed {
        let Feed::Mirror { chain, source } = listing.feed else {
            unreachable!()
        };
        let address = deploy_mirror(&robinhood, &mainnet, listing.token.symbol, source, me).await;
        mirrors.feeds.insert(
            listing.token.symbol.to_string(),
            MirrorRecord {
                address,
                source,
                source_chain_id: chain,
            },
        );
        deal(
            &robinhood,
            listing.token.address,
            me,
            U256::from(10u8) * U256::from(10u8).pow(U256::from(18u8)),
        )
        .await;
    }
    let mut chains = vec![
        ChainMarket::new(
            "evm:46630".into(),
            Endpoints::new(&robinhood_url, signer.clone()).unwrap(),
            &ROBINHOOD_TESTNET,
            &mirrors,
            true,
        )
        .unwrap(),
    ];

    // Base Sepolia: WETH wrapped from the fork's coin, USDC dealt.
    let base_fork = base_url.as_deref().map(fork);
    if let Some((_, url)) = &base_fork {
        let base = provider(url, &signer);
        let weth = BASE_SEPOLIA_VENUE.listed[0].token.address;
        let receipt = base
            .send_transaction(
                TransactionRequest::default()
                    .with_to(weth)
                    .with_value(U256::from(10u8).pow(U256::from(18u8))),
            )
            .await
            .unwrap()
            .get_receipt()
            .await
            .unwrap();
        assert!(receipt.status(), "wrapping ETH");
        deal(
            &base,
            BASE_SEPOLIA_VENUE.dollar.address,
            me,
            U256::from(2_000_000_000u64),
        )
        .await;
        chains.push(
            ChainMarket::new(
                "evm:84532".into(),
                Endpoints::new(url, signer.clone()).unwrap(),
                &BASE_SEPOLIA_VENUE,
                &MirrorFeeds::default(),
                false,
            )
            .unwrap(),
        );
    }

    let settings = Settings {
        // Every pool all the way in one round.
        max_trade_usd: 1_000_000.0,
        // A test network's feed may be days old at a weekend.
        max_reference_age: Duration::from_secs(30 * 86_400),
        ..Settings::default()
    };
    let band = settings.band_bps;
    let health = Arc::new(Mutex::new(Health::default()));
    let mut market = Market {
        account: me,
        source: mainnet.clone(),
        chains,
        settings,
        health: health.clone(),
    };
    market.round().await;
    {
        let health = health.lock().unwrap();
        for (key, chain) in &health.chains {
            assert_eq!(chain.error, None, "{key}");
        }
        let robinhood = &health.chains["evm:46630"];
        assert_eq!(robinhood.feeds.len(), 4);
        for feed in &robinhood.feeds {
            assert_eq!(
                feed.mirror_round, feed.source_round,
                "{} posted",
                feed.symbol
            );
            assert!(feed.last_post.is_some());
        }
    }

    // Every pool is within the band of its feed (the mirror's on Robinhood
    // Chain, Chainlink's own on Base Sepolia).
    for chain in &market.chains {
        let venue = if chain.key == "evm:46630" {
            &ROBINHOOD_TESTNET
        } else {
            &BASE_SEPOLIA_VENUE
        };
        let provider = chain.endpoints.provider();
        for kept in &chain.pools {
            let premium = premium(
                provider,
                venue,
                kept.feed,
                kept.pool,
                kept.token.address,
                kept.token.decimals,
            )
            .await;
            assert!(
                premium.abs() < band,
                "{} on {} is {premium:.1} bps off its feed",
                kept.token.symbol,
                chain.key
            );
        }
    }

    // The next round has nothing to post and nothing to trade.
    let nonces: Vec<u64> = futures_nonces(&market).await;
    market.round().await;
    assert_eq!(futures_nonces(&market).await, nonces, "nothing sent");
}

async fn futures_nonces(market: &Market) -> Vec<u64> {
    let mut nonces = Vec::new();
    for chain in &market.chains {
        nonces.push(
            chain
                .endpoints
                .provider()
                .get_transaction_count(market.account)
                .await
                .unwrap(),
        );
    }
    nonces
}
