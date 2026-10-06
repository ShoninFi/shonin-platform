//! An agent sizes its orders against the account the driver reads for it,
//! and that account names each spot balance by a token. Anyone may send a
//! spot token to any account on the venue, and the venue (its testnet above
//! all) lists look-alike tokens under a name already taken. A balance of a
//! look-alike must not reach the agent under the same name as the real
//! token's: the agent could not tell 900 of somebody's own token from 900
//! of the one it trades.

use serde_json::json;
use shonin_hyperliquid::Markets;
use shonin_hyperliquid::events::state_from;

#[test]
fn a_look_alike_token_s_balance_is_not_reported_under_the_real_token_s_name() {
    let meta = json!({"universe": [
        {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
    ]});
    let spot_meta = json!({
        "tokens": [
            {"name": "USDC", "index": 0, "szDecimals": 8, "weiDecimals": 8, "tokenId": "0xaa"},
            {"name": "HYPE", "index": 1105, "szDecimals": 2, "weiDecimals": 8, "tokenId": "0xbb"},
            {"name": "HYPE", "index": 4242, "szDecimals": 0, "weiDecimals": 5, "tokenId": "0xcc"}
        ],
        "universe": [
            {"name": "@1035", "tokens": [1105, 0], "index": 1035},
            {"name": "@4000", "tokens": [4242, 0], "index": 4000}
        ]
    });
    let markets = Markets::from_meta(&meta, &spot_meta).unwrap();

    // The account holds 0.3 of the real token; somebody sent it 900 of the
    // look-alike.
    let perp = json!({
        "marginSummary": {"accountValue": "100"},
        "withdrawable": "80",
        "assetPositions": []
    });
    let spot = json!({"balances": [
        {"coin": "USDC", "token": 0, "total": "14.6", "hold": "0.0"},
        {"coin": "HYPE", "token": 1105, "total": "0.3", "hold": "0.0"},
        {"coin": "HYPE", "token": 4242, "total": "900", "hold": "0.0"}
    ]});

    let state = state_from(&markets, &perp, &spot, &json!([])).unwrap();

    let named_hype: Vec<_> = state
        .spot
        .iter()
        .filter(|balance| balance.token == "HYPE")
        .collect();
    assert!(
        named_hype.len() <= 1,
        "two different tokens reached the agent under one name: {named_hype:?}"
    );
    // As with their pairs, a name two tokens share names neither: each
    // balance is named by the venue's number for its token. The dollar is
    // the dollar by its number, and keeps its name.
    let named = |token: &str| state.spot.iter().find(|b| b.token == token).unwrap().total;
    assert_eq!(named("HYPE:1105"), "0.3".parse().unwrap());
    assert_eq!(named("HYPE:4242"), "900".parse().unwrap());
    assert_eq!(named("USDC"), "14.6".parse().unwrap());
}
