//! Robinhood Chain's contracts, by network, as checked on-chain on 6 October
//! 2026.
//!
//! Mainnet (chain 4663) has everything the driver uses. The testnet (46630)
//! has Safe, Zodiac Roles v2 and its own stock tokens, but no Chainlink feeds,
//! no official Uniswap and no USDG, so a driver there can hold and scope but
//! not value or swap.

use alloy::primitives::{Address, address};

/// Robinhood Chain mainnet's chain id.
pub const MAINNET: u64 = 4663;
/// Its public testnet's.
pub const TESTNET: u64 = 46630;

/// Safe v1.4.1 and Zodiac Roles v2, at the same addresses on both networks.
pub const SAFE_SINGLETON: Address = address!("29fcB43b46531BcA003ddC8FCB67FFE91900C762");
pub const SAFE_PROXY_FACTORY: Address = address!("4e1DCf7AD4e460CfD30791CCC4F9c8a4f820ec67");
pub const ROLES_MASTERCOPY: Address = address!("9646fDAD06d3e24444381f44362a3B0eB343D337");
pub const MODULE_PROXY_FACTORY: Address = address!("000000000000aDdB49795b0f9bA5BC298cDda236");

/// Mainnet's dollar (USDG, 6 decimals) and Uniswap's SwapRouter02.
pub const USDG: Address = address!("5fc5360D0400a0Fd4f2af552ADD042D716F1d168");
pub const SWAP_ROUTER_02: Address = address!("caf681a66d020601342297493863e78c959e5cb2");

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

/// The testnet's stock tokens (18 decimals; no feeds, no pools).
pub const TESTNET_STOCKS: &[(&str, Address)] = &[
    ("TSLA", address!("C9f9c86933092BbbfFF3CCb4b105A4A94bf3Bd4E")),
    ("AMZN", address!("5884aD2f920c162CFBbACc88C9C51AA75eC09E02")),
    ("NFLX", address!("3b8262A63d25f0477c4DDE23F83cfe22Cb768C93")),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_mainnet_has_feeds_and_they_are_found_by_symbol() {
        assert_eq!(feed(MAINNET, "nvda"), Some(MAINNET_STOCKS[0].feed));
        assert_eq!(feed(TESTNET, "NVDA"), None);
        assert_eq!(feed(MAINNET, "DOGE"), None);
    }
}
