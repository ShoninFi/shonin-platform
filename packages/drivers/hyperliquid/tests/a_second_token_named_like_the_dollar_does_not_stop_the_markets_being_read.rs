//! The dollar on HyperCore is the token the venue numbers 0: the driver
//! contract and the executor both name it by that number. The venue (its
//! testnet above all) lets a second token be listed under a name already
//! taken, and a second token named like the dollar is not the dollar. The
//! venue's markets must still be read with it listed: read by name, every
//! executor that starts afterwards fails to read the markets at all, and no
//! Hyperliquid vault is served until the look-alike is gone.

use alloy::primitives::Address;
use serde_json::json;
use shonin_hyperliquid::Markets;
use shonin_hyperliquid::agent::Allowlist;

#[test]
fn a_second_token_named_like_the_dollar_does_not_stop_the_markets_being_read() {
    let meta = json!({"universe": [
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
    ]});
    let spot = json!({
        "tokens": [
            {"name": "USDC", "index": 0, "szDecimals": 8, "weiDecimals": 8, "tokenId": "0xaa"},
            {"name": "HYPE", "index": 1105, "szDecimals": 2, "weiDecimals": 8, "tokenId": "0xbb"},
            // Somebody's own token, listed under the dollar's name.
            {"name": "USDC", "index": 4242, "szDecimals": 0, "weiDecimals": 5, "tokenId": "0xcc"}
        ],
        "universe": [
            {"name": "@1035", "tokens": [1105, 0], "index": 1035},
            {"name": "@4000", "tokens": [4242, 0], "index": 4000}
        ]
    });

    let markets = Markets::from_meta(&meta, &spot);

    let markets = match markets {
        Ok(markets) => markets,
        Err(error) => panic!(
            "a token listed under the dollar's name stopped the venue's markets being read: {error:#}"
        ),
    };
    // The dollar is still the token the venue numbers 0, and the pinned pair
    // against it is still traded.
    assert_eq!(markets.usdc_wire, "USDC:0xaa");
    let allowlist =
        Allowlist::from_driver(Address::repeat_byte(0x55), markets, &[0], &[1035]).unwrap();
    assert!(allowlist.missing.is_empty(), "{:?}", allowlist.missing);
    assert_eq!(allowlist.markets.spot_pair(1035).unwrap().asset, 11_035);
}
