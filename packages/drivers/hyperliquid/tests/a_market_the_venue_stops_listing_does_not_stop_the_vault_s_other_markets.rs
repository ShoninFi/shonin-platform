//! A vault keeps its driver for good, and the driver's list of markets is
//! fixed when it is deployed. The venue's own list is not: it delists perps,
//! turns some isolated-only, and (on testnet) lists look-alike tokens under a
//! name already taken. When one market the driver pins stops being one an
//! agent may trade, the agent key is still held to the others, and an
//! action in the missing market is refused with the reason. A token is told
//! apart by the venue's number for it, as the driver pins it, never by its
//! name.

use alloy::primitives::Address;
use serde_json::{Value, json};
use shonin_hyperliquid::Markets;
use shonin_hyperliquid::agent::Allowlist;
use shonin_hyperliquid::payload::{Action, Order, Side, Tif};

fn spot() -> Value {
    json!({
        "tokens": [
            {"name": "USDC", "index": 0, "szDecimals": 8, "weiDecimals": 8, "tokenId": "0xaa"},
            {"name": "HYPE", "index": 1105, "szDecimals": 2, "weiDecimals": 8, "tokenId": "0xbb"}
        ],
        "universe": [
            {"name": "@1035", "tokens": [1105, 0], "index": 1035}
        ]
    })
}

fn order(market: &str, price: &str) -> Action {
    Action::Place(Order {
        market: market.to_string(),
        side: Side::Buy,
        size: "0.5".parse().unwrap(),
        limit_price: price.parse().unwrap(),
        tif: Tif::Gtc,
        reduce_only: false,
        client_id: None,
    })
}

/// The driver pins perps 0 and 1 and the HYPE pair; the venue has since
/// delisted perp 1.
#[test]
fn a_perp_the_venue_delists_does_not_take_the_vault_s_other_markets_with_it() {
    let meta = json!({"universe": [
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25},
        {"name": "OLD", "szDecimals": 2, "maxLeverage": 3, "isDelisted": true}
    ]});
    let markets = Markets::from_meta(&meta, &spot()).unwrap();

    let allowlist = Allowlist::from_driver(Address::repeat_byte(0x55), markets, &[0, 1], &[1035]);

    let allowlist = allowlist.unwrap();
    assert!(allowlist.check(&order("ETH", "2700"), 1).is_ok());
    assert!(allowlist.check(&order("HYPE/USDC", "22.5"), 1).is_ok());
    // The delisted perp itself is not opened, and the agent hears why.
    let refused = allowlist.check(&order("OLD", "1"), 1).unwrap_err();
    assert!(refused.to_string().contains("delisted"), "{refused:#}");
    assert!(allowlist.missing.contains_key("OLD"));
}

/// The same when the venue turns a pinned perp isolated-only.
#[test]
fn a_perp_turned_isolated_only_does_not_take_the_vault_s_other_markets_with_it() {
    let meta = json!({"universe": [
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25},
        {"name": "ISO", "szDecimals": 1, "maxLeverage": 3, "onlyIsolated": true}
    ]});
    let markets = Markets::from_meta(&meta, &spot()).unwrap();

    let allowlist = Allowlist::from_driver(Address::repeat_byte(0x55), markets, &[0, 1], &[1035]);

    let allowlist = allowlist.unwrap();
    assert!(allowlist.check(&order("ETH", "2700"), 1).is_ok());
    let refused = allowlist.check(&order("ISO", "1"), 1).unwrap_err();
    assert!(refused.to_string().contains("isolated"), "{refused:#}");
}

/// A pinned number the venue doesn't list at all is refused by that number,
/// and the rest are still traded.
#[test]
fn a_perp_the_venue_no_longer_lists_at_all_does_not_take_the_vault_s_other_markets_with_it() {
    let meta = json!({"universe": [
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
    ]});
    let markets = Markets::from_meta(&meta, &spot()).unwrap();

    let allowlist =
        Allowlist::from_driver(Address::repeat_byte(0x55), markets, &[0, 9], &[1035, 77]).unwrap();

    assert!(allowlist.check(&order("ETH", "2700"), 1).is_ok());
    assert!(allowlist.check(&order("HYPE/USDC", "22.5"), 1).is_ok());
    assert!(allowlist.missing.contains_key("perp 9"));
    assert!(allowlist.missing.contains_key("spot pair 77"));
}

/// The driver pins the HYPE pair by the venue's number for it, which no
/// other token shares. Someone lists a second token under the same name: the
/// pinned pair is still the same pair.
#[test]
fn a_look_alike_token_does_not_take_the_pinned_pair_or_the_vault_with_it() {
    let meta = json!({"universe": [
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
    ]});
    let mut spot = spot();
    spot["tokens"].as_array_mut().unwrap().push(json!(
        {"name": "HYPE", "index": 4242, "szDecimals": 0, "weiDecimals": 5, "tokenId": "0xcc"}
    ));
    spot["universe"].as_array_mut().unwrap().push(json!(
        {"name": "@4000", "tokens": [4242, 0], "index": 4000}
    ));
    let markets = Markets::from_meta(&meta, &spot).unwrap();

    let allowlist =
        Allowlist::from_driver(Address::repeat_byte(0x55), markets, &[0], &[1035]).unwrap();

    assert!(allowlist.missing.is_empty());
    // The name is shared, so each pair goes by the venue's own name for it:
    // the pinned one is traded, and it is the pair the driver pins.
    let pinned = allowlist.markets.spot_pair(1035).unwrap();
    assert_eq!(pinned.name, "@1035");
    assert_eq!(pinned.asset, 11_035);
    assert!(allowlist.check(&order("@1035", "22.5"), 1).is_ok());
    // The look-alike is not the driver's.
    assert!(allowlist.check(&order("@4000", "1"), 1).is_err());
    assert!(allowlist.check(&order("HYPE/USDC", "22.5"), 1).is_err());
}
