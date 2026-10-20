//! The chains the driver trades on, and what it lists on each, as checked
//! on-chain in October 2026.
//!
//! Robinhood Chain mainnet (4663) has everything the driver uses: stock
//! tokens, Chainlink's per-token feeds, USDG and Uniswap v3. Its testnet
//! (46630) has its own stock tokens, USDG, Safe, Zodiac Roles v2 and an
//! unmodified Uniswap v3, but no Chainlink feeds: each listed stock is
//! priced by a mirror of its mainnet feed, a contract the testnet deploy
//! puts there ([`Feed::Mirror`]) and `testnet-market` keeps posting the
//! mainnet rounds to. Base Sepolia (84532) has the same Safe and Roles, an
//! official Uniswap v3, Circle's USDC and Chainlink's own ETH/USD feed, so
//! the same driver lists WETH there.
//!
//! Everything here is a contract someone else deployed (the tokens, the
//! dollars, Uniswap, Chainlink's feeds). What the platform deploys on a test
//! network is never written down: a mirror feed's address comes from the
//! record the deploy writes beside the chain's deployment file
//! ([`MirrorFeeds`]), and a pool's from its factory, by the CREATE2 rule
//! every Uniswap v3 factory follows ([`pool_address`]). A listing is
//! resolved to addresses once, at runtime ([`Venue::resolve`]), and the
//! driver contract is built from that: it reads a mirror exactly as it
//! reads a Chainlink feed, and only the address differs.

use std::collections::BTreeMap;

use alloy::primitives::{Address, B256, address, b256, keccak256};
use alloy::sol_types::SolValue;
use serde::{Deserialize, Serialize};

/// Robinhood Chain mainnet's chain id.
pub const MAINNET: u64 = 4663;
/// Its public testnet's.
pub const TESTNET: u64 = 46630;
/// Base's public testnet's.
pub const BASE_SEPOLIA: u64 = 84532;

/// Safe v1.4.1 and Zodiac Roles v2, at the same addresses on every chain
/// the driver serves.
pub const SAFE_SINGLETON: Address = address!("29fcB43b46531BcA003ddC8FCB67FFE91900C762");
pub const SAFE_PROXY_FACTORY: Address = address!("4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67");
pub const ROLES_MASTERCOPY: Address = address!("9646fDAD06d3e24444381f44362a3B0eB343D337");
pub const MODULE_PROXY_FACTORY: Address = address!("000000000000aDdB49795b0f9bA5BC298cDda236");

/// Mainnet's dollar (USDG, 6 decimals) and Uniswap's SwapRouter02.
pub const USDG: Address = address!("5fc5360D0400a0Fd4f2af552ADD042D716F1d168");
pub const SWAP_ROUTER_02: Address = address!("caf681a66d020601342297493863e78c959e5cb2");

/// The hash of a Uniswap v3 pool's creation code: a pool's address follows
/// from it, its factory, its two tokens and its fee, on every chain whose
/// factory is Uniswap's own, unmodified.
pub const POOL_INIT_CODE_HASH: B256 =
    b256!("e34f199b19b2b4f47f68442619d555527d244f78a3297ea89325f843f87b8b54");

/// A stock token on mainnet: its token, its Chainlink feed (8 decimals,
/// multiplier applied) and its deepest USDG pool on Uniswap v3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stock {
    pub symbol: &'static str,
    pub token: Address,
    pub feed: Address,
    pub pool: Address,
}

/// The stock tokens whose token, feed and pool are confirmed.
pub const MAINNET_STOCKS: &[Stock] = &[
    Stock {
        symbol: "NVDA",
        token: address!("d0601CE157Db5bdC3162BbaC2a2C8aF5320D9EEC"),
        feed: address!("379EC4f7C378F34a1B47E4F3cbeBCbAC3E8E9F15"),
        pool: address!("d4EB21209C4D6093f80B5b84f5C45cc093EA14a3"),
    },
    Stock {
        symbol: "TSLA",
        token: address!("322F0929c4625eD5bAd873c95208D54E1c003b2d"),
        feed: address!("4A1166a659A55625345e9515b32adECea5547C38"),
        pool: address!("c4f0172D6ac8DD294Dd1137D047d5E1893760236"),
    },
    Stock {
        symbol: "AAPL",
        token: address!("aF3D76f1834A1d425780943C99Ea8A608f8a93f9"),
        feed: address!("6B22A786bAa607d76728168703a39Ea9C99f2cD0"),
        pool: address!("Aae0d815EE56e4092a5E5C2911E676Fea50B2d6D"),
    },
];

/// Chainlink's per-token feeds on mainnet, by the token's symbol (the
/// shared Chainlink stream serves these).
pub const MAINNET_FEEDS: &[(&str, Address)] = &[
    ("AAPL", address!("6B22A786bAa607d76728168703a39Ea9C99f2cD0")),
    ("AMD", address!("943A29E7ae51A4798823ca9eEd2ed533B2A22C72")),
    ("AMZN", address!("D5a1508ceD74c084eBf3cBe853e2C968fB2a651C")),
    ("COIN", address!("A3a468A452940B7D6b69991207B508c609a98Ef2")),
    (
        "GOOGL",
        address!("F6f373a037c30F0e5010d854385cA89185AE638b"),
    ),
    ("META", address!("7C38C00C30BEe9378381E7B6135d7283356D71b1")),
    ("MSFT", address!("45C3C877C15E6BA2EBB19eA114Ea508d14C1Af2E")),
    ("MSTR", address!("396118bdFB181e6240E74D243F266B061c0edc3D")),
    ("NVDA", address!("379EC4f7C378F34a1B47E4F3cbeBCbAC3E8E9F15")),
    ("PLTR", address!("820ABedFF239034956B7A9d2F0a331f9F075eB4c")),
    ("QQQ", address!("80901d846d5D7B030F26B480776EE3b29374C2ae")),
    ("SPY", address!("319724394D3A0e3669269846abE664Cd621f9f6A")),
    ("TSLA", address!("4A1166a659A55625345e9515b32adECea5547C38")),
    ("USDG", address!("61B7e5650328764B076A108EFF5fa7282a1B9aD2")),
];

/// The Chainlink feed for `symbol` on `chain`, if there is one.
pub fn feed(chain: u64, symbol: &str) -> Option<Address> {
    if chain != MAINNET {
        return None;
    }
    MAINNET_FEEDS
        .iter()
        .find(|(listed, _)| listed.eq_ignore_ascii_case(symbol))
        .map(|(_, feed)| *feed)
}

/// A token, as the driver needs to know it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub symbol: &'static str,
    pub address: Address,
    pub decimals: u8,
}

/// Where a listed token's price comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feed {
    /// Chainlink's own feed on the chain.
    Chainlink(Address),
    /// A mirror of Chainlink's feed `source` on chain `chain`, deployed on
    /// the test network and found in its [`MirrorFeeds`] record.
    Mirror { chain: u64, source: Address },
}

/// One token the driver lists on a chain, against the chain's dollar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listing {
    pub token: Token,
    pub feed: Feed,
    /// The fee tier of its pool against the dollar.
    pub fee: u32,
}

/// Uniswap v3 on a chain: the factory pools are found from, the router
/// and quoter swaps go through, and the position manager liquidity is
/// added through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Uniswap {
    pub factory: Address,
    pub router: Address,
    pub quoter: Address,
    pub positions: Address,
}

/// Everything the driver trades on one chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Venue {
    pub chain: u64,
    pub label: &'static str,
    /// The name the driver is approved under on this chain's registry, as
    /// `config/drivers.toml` lists it (one chain to a name).
    pub driver: &'static str,
    pub dollar: Token,
    pub uniswap: Uniswap,
    pub listed: &'static [Listing],
}

/// The feeds' decimals: Chainlink's USD feeds' and the mirrors'.
pub const FEED_DECIMALS: u8 = 8;

const fn mirror(symbol: &'static str, token: Address, source: Address) -> Listing {
    Listing {
        token: Token {
            symbol,
            address: token,
            decimals: 18,
        },
        feed: Feed::Mirror {
            chain: MAINNET,
            source,
        },
        fee: 500,
    }
}

/// Robinhood Chain testnet: four of Robinhood's stock tokens (each with a
/// mainnet feed readable with no key), against USDG, each priced by a
/// mirror of its mainnet feed. The tokens come from Robinhood's faucet; USDG
/// can be minted by anyone.
pub const ROBINHOOD_TESTNET: Venue = Venue {
    chain: TESTNET,
    label: "Robinhood Chain testnet",
    driver: crate::payload::DRIVER,
    dollar: Token {
        symbol: "USDG",
        address: address!("293b337712D4312776a3a2D292F44410E7873bAd"),
        decimals: 6,
    },
    uniswap: Uniswap {
        factory: address!("9FA847859602f73320DA5fA6cF1D982c858D798B"),
        router: address!("93353D8404098CfA60De14C61D35a8F68DaBd060"),
        quoter: address!("A6Ed19a0EEeb373b4fCc34fE046d123190245Cf6"),
        positions: address!("F06755645Ad127333B74BBc3c1ca5Fa9d9B5a861"),
    },
    listed: &[
        mirror(
            "TSLA",
            address!("C9f9c86933092BbbfFF3CCb4b105A4A94bf3Bd4E"),
            address!("4A1166a659A55625345e9515b32adECea5547C38"),
        ),
        mirror(
            "AMD",
            address!("71178BAc73cBeb415514eB542a8995b82669778d"),
            address!("943A29E7ae51A4798823ca9eEd2ed533B2A22C72"),
        ),
        mirror(
            "AMZN",
            address!("5884aD2f920c162CFBbACc88C9C51AA75eC09E02"),
            address!("D5a1508ceD74c084eBf3cBe853e2C968fB2a651C"),
        ),
        mirror(
            "PLTR",
            address!("1FBE1a0e43594b3455993B5dE5Fd0A7A266298d0"),
            address!("820ABedFF239034956B7A9d2F0a331f9F075eB4c"),
        ),
    ],
};

/// Base Sepolia: WETH against Circle's USDC on the official Uniswap v3,
/// priced by Base Sepolia's own Chainlink ETH/USD feed, as a vault there
/// values it.
pub const BASE_SEPOLIA_VENUE: Venue = Venue {
    chain: BASE_SEPOLIA,
    label: "Base Sepolia",
    driver: crate::payload::BASE_DRIVER,
    dollar: Token {
        symbol: "USDC",
        address: address!("036CbD53842c5426634e7929541eC2318f3dCF7e"),
        decimals: 6,
    },
    uniswap: Uniswap {
        factory: address!("4752ba5DBc23f44D87826276BF6Fd6b1C372aD24"),
        router: address!("94cC0AaC535CCDB3C01d6787D6413C739ae12bc4"),
        quoter: address!("C5290058841028F1614F3A6F0F5816cAd0df5E27"),
        positions: address!("27F971cb582BF9E50F397e4d29a5C7A34f11faA2"),
    },
    listed: &[Listing {
        token: Token {
            symbol: "WETH",
            address: address!("4200000000000000000000000000000000000006"),
            decimals: 18,
        },
        feed: Feed::Chainlink(address!("4aDC67696bA383F43DD60A9e78F2C97Fbbfc7cb1")),
        fee: 500,
    }],
};

/// The venue the driver trades on `chain`, on a test network.
pub fn test_venue(chain: u64) -> Option<&'static Venue> {
    [&ROBINHOOD_TESTNET, &BASE_SEPOLIA_VENUE]
        .into_iter()
        .find(|venue| venue.chain == chain)
}

/// The address of `factory`'s Uniswap v3 pool of `a` and `b` at `fee`,
/// whether or not it has been created.
pub fn pool_address(factory: Address, a: Address, b: Address, fee: u32) -> Address {
    let (token0, token1) = if a < b { (a, b) } else { (b, a) };
    let salt = keccak256((token0, token1, alloy::primitives::aliases::U24::from(fee)).abi_encode());
    factory.create2(salt, POOL_INIT_CODE_HASH)
}

/// The mirror feeds a test network's deploy put on one chain, by the
/// listed token's symbol: the file `mirror-feeds-<chain>.json` beside the
/// chain's deployment file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorFeeds {
    pub chain_id: u64,
    pub feeds: BTreeMap<String, MirrorRecord>,
}

/// One mirror feed: where it is, and the mainnet feed it copies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorRecord {
    pub address: Address,
    pub source: Address,
    pub source_chain_id: u64,
}

impl MirrorFeeds {
    /// The record's file name in a deployments directory, for an EVM chain.
    pub fn file_name(chain_id: u64) -> String {
        format!("mirror-feeds-evm-{chain_id}.json")
    }

    /// Read the record for `chain_id` from a deployments directory; an
    /// empty record if there is none.
    pub fn read(dir: &std::path::Path, chain_id: u64) -> anyhow::Result<Self> {
        let path = dir.join(Self::file_name(chain_id));
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let record: Self = serde_json::from_str(&text)
                    .map_err(|error| anyhow::anyhow!("reading {}: {error}", path.display()))?;
                if record.chain_id != chain_id {
                    anyhow::bail!(
                        "{} is chain {}'s record, not {chain_id}'s",
                        path.display(),
                        record.chain_id
                    );
                }
                Ok(record)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                chain_id,
                feeds: BTreeMap::new(),
            }),
            Err(error) => Err(anyhow::anyhow!("reading {}: {error}", path.display())),
        }
    }

    /// Write the record into a deployments directory.
    pub fn write(&self, dir: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(Self::file_name(self.chain_id));
        std::fs::write(&path, serde_json::to_string_pretty(self)? + "\n")
            .map_err(|error| anyhow::anyhow!("writing {}: {error}", path.display()))?;
        Ok(path)
    }
}

/// One listing with every address known: what the driver contract is built
/// from, and what `testnet-market` keeps at the feed's price.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub token: Token,
    pub feed: Address,
    pub pool: Address,
    pub fee: u32,
}

impl Venue {
    /// Every listing's feed and pool. A mirror feed comes from `mirrors`,
    /// and must copy the mainnet feed the listing names; one that isn't
    /// recorded is an error naming it.
    pub fn resolve(&self, mirrors: &MirrorFeeds) -> anyhow::Result<Vec<Resolved>> {
        self.listed
            .iter()
            .map(|listing| {
                let feed = match listing.feed {
                    Feed::Chainlink(feed) => feed,
                    Feed::Mirror { chain, source } => {
                        let symbol = listing.token.symbol;
                        let record = mirrors.feeds.get(symbol).ok_or_else(|| {
                            anyhow::anyhow!(
                                "no mirror feed for {symbol} on chain {}: deploy it first",
                                self.chain
                            )
                        })?;
                        if mirrors.chain_id != self.chain
                            || record.source != source
                            || record.source_chain_id != chain
                        {
                            anyhow::bail!(
                                "the mirror feed recorded for {symbol} copies {} on chain {}, not {source} on chain {chain}",
                                record.source,
                                record.source_chain_id
                            );
                        }
                        record.address
                    }
                };
                Ok(Resolved {
                    token: listing.token,
                    feed,
                    pool: pool_address(
                        self.uniswap.factory,
                        listing.token.address,
                        self.dollar.address,
                        listing.fee,
                    ),
                    fee: listing.fee,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uniswap's factory on Robinhood Chain mainnet.
    const MAINNET_FACTORY: Address = address!("1f7d7550B1b028f7571E69A784071F0205FD2EfA");

    #[test]
    fn only_mainnet_has_feeds_and_they_are_found_by_symbol() {
        assert_eq!(feed(MAINNET, "nvda"), Some(MAINNET_STOCKS[0].feed));
        assert_eq!(feed(TESTNET, "NVDA"), None);
        assert_eq!(feed(MAINNET, "DOGE"), None);
    }

    #[test]
    fn a_pool_s_address_follows_from_its_factory_tokens_and_fee_in_either_order() {
        for stock in MAINNET_STOCKS.iter().take(2) {
            assert_eq!(
                pool_address(MAINNET_FACTORY, stock.token, USDG, 500),
                stock.pool,
                "{}",
                stock.symbol
            );
            assert_eq!(
                pool_address(MAINNET_FACTORY, USDG, stock.token, 500),
                stock.pool
            );
        }
        assert_ne!(
            pool_address(MAINNET_FACTORY, USDG, MAINNET_STOCKS[0].token, 3000),
            MAINNET_STOCKS[0].pool
        );
    }

    #[test]
    fn each_testnet_stock_mirrors_its_own_mainnet_feed() {
        for listing in ROBINHOOD_TESTNET.listed {
            let Feed::Mirror { chain, source } = listing.feed else {
                panic!("{} is not mirrored", listing.token.symbol);
            };
            assert_eq!(chain, MAINNET);
            assert_eq!(feed(MAINNET, listing.token.symbol), Some(source));
            assert_eq!(listing.token.decimals, 18);
        }
        let symbols: Vec<_> = ROBINHOOD_TESTNET
            .listed
            .iter()
            .map(|listing| listing.token.symbol)
            .collect();
        assert_eq!(symbols, ["TSLA", "AMD", "AMZN", "PLTR"]);
    }

    fn mirrors() -> MirrorFeeds {
        MirrorFeeds {
            chain_id: TESTNET,
            feeds: ROBINHOOD_TESTNET
                .listed
                .iter()
                .enumerate()
                .map(|(i, listing)| {
                    let Feed::Mirror { chain, source } = listing.feed else {
                        unreachable!()
                    };
                    (
                        listing.token.symbol.to_string(),
                        MirrorRecord {
                            address: Address::repeat_byte(i as u8 + 1),
                            source,
                            source_chain_id: chain,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn a_testnet_listing_resolves_to_the_recorded_mirrors_and_the_factory_s_pools() {
        let resolved = ROBINHOOD_TESTNET.resolve(&mirrors()).unwrap();
        assert_eq!(resolved.len(), 4);
        for (i, stock) in resolved.iter().enumerate() {
            assert_eq!(stock.feed, Address::repeat_byte(i as u8 + 1));
            assert_eq!(
                stock.pool,
                pool_address(
                    ROBINHOOD_TESTNET.uniswap.factory,
                    ROBINHOOD_TESTNET.dollar.address,
                    stock.token.address,
                    500
                )
            );
        }
        let base = BASE_SEPOLIA_VENUE.resolve(&MirrorFeeds::default()).unwrap();
        assert_eq!(base.len(), 1);
        assert_eq!(
            Feed::Chainlink(base[0].feed),
            BASE_SEPOLIA_VENUE.listed[0].feed
        );
    }

    #[test]
    fn a_missing_or_mismatched_mirror_is_refused() {
        let mut missing = mirrors();
        missing.feeds.remove("AMD");
        assert!(ROBINHOOD_TESTNET.resolve(&missing).is_err());

        let mut wrong = mirrors();
        wrong.feeds.get_mut("TSLA").unwrap().source = MAINNET_STOCKS[0].feed;
        assert!(ROBINHOOD_TESTNET.resolve(&wrong).is_err());

        let mut elsewhere = mirrors();
        elsewhere.chain_id = BASE_SEPOLIA;
        assert!(ROBINHOOD_TESTNET.resolve(&elsewhere).is_err());
    }

    #[test]
    fn a_mirror_record_round_trips_through_its_file() {
        let dir = std::env::temp_dir().join(format!("mirror-feeds-{}", std::process::id()));
        assert!(MirrorFeeds::read(&dir, TESTNET).unwrap().feeds.is_empty());
        let record = mirrors();
        record.write(&dir).unwrap();
        assert_eq!(MirrorFeeds::read(&dir, TESTNET).unwrap(), record);
        assert!(
            MirrorFeeds::read(&dir, BASE_SEPOLIA)
                .unwrap()
                .feeds
                .is_empty()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_test_venues_are_found_by_chain_and_mainnet_has_none() {
        assert_eq!(test_venue(TESTNET), Some(&ROBINHOOD_TESTNET));
        assert_eq!(test_venue(BASE_SEPOLIA), Some(&BASE_SEPOLIA_VENUE));
        assert_eq!(test_venue(MAINNET), None);
    }
}
