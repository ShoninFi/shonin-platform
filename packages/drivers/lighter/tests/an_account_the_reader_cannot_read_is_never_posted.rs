//! The post values the station: what the exchange's API says the accounts
//! are worth, and what they hold. An answer the reader can't make sense of
//! (a figure that is missing, or written as a number rather than as text)
//! must stop the post, as a position in a market it doesn't know does. It
//! must never be posted as an account worth nothing, a position that isn't
//! there, or a short as a long.

use alloy::primitives::U256;
use serde_json::{Value, json};
use shonin_lighter::Markets;
use shonin_lighter::report::{LighterReport, OnChain, build};

fn markets() -> Markets {
    Markets::from_order_books(&json!({"order_books": [
        {"symbol": "SPY", "market_id": 26, "market_type": "perp", "status": "active",
         "supported_size_decimals": 4, "supported_price_decimals": 2, "min_base_amount": "0.0100"}
    ]}))
    .unwrap()
}

fn post(account: Value) -> anyhow::Result<LighterReport> {
    build(OnChain::default(), &[account], &[], &markets(), &[])
}

/// An account the reader reads whole: worth 1000.5, a short of 1.5 SPY.
fn readable() -> Value {
    json!({
        "index": 40518, "total_asset_value": "1000.5", "available_balance": "250.25",
        "positions": [{"market_id": 26, "position": "1.5000", "sign": -1}],
        "shares": []
    })
}

/// The readable account with one field of it (a path of keys and indexes)
/// set to `value`, or removed for `None`.
fn with(path: &[&str], value: Option<Value>) -> Value {
    let mut account = readable();
    let mut at = &mut account;
    for key in &path[..path.len() - 1] {
        at = match key.parse::<usize>() {
            Ok(index) => &mut at[index],
            Err(_) => &mut at[*key],
        };
    }
    let last = path[path.len() - 1];
    match (value, last.parse::<usize>()) {
        (Some(value), Ok(index)) => at[index] = value,
        (Some(value), Err(_)) => at[last] = value,
        (None, _) => {
            at.as_object_mut().unwrap().remove(last);
        }
    }
    account
}

#[test]
fn the_readable_account_is_posted_as_read() {
    let report = post(readable()).unwrap();
    assert_eq!(report.equity, U256::from(1_000_500_000u64));
    assert_eq!(report.positions[0].size, -15_000);
}

#[test]
fn an_account_whose_worth_is_missing_is_not_posted_as_worth_nothing() {
    let built = post(with(&["total_asset_value"], None));
    assert!(
        built.is_err(),
        "an account whose worth the answer doesn't give was posted as worth {}",
        built.map(|report| report.equity).unwrap_or(U256::MAX)
    );
}

#[test]
fn an_account_whose_worth_is_written_as_a_number_is_not_posted_as_worth_nothing() {
    let built = post(with(&["total_asset_value"], Some(json!(1000.5))));
    let equity = built.map(|report| report.equity);
    assert!(
        !matches!(equity, Ok(worth) if worth.is_zero()),
        "an account worth 1000.5 was posted as worth nothing"
    );
    assert!(
        equity.is_err(),
        "a figure written in another type stops the post"
    );
}

#[test]
fn an_account_whose_worth_cannot_be_read_is_not_posted() {
    for worth in [json!("about 1000"), json!(""), json!(null)] {
        assert!(
            post(with(&["total_asset_value"], Some(worth.clone()))).is_err(),
            "{worth}"
        );
    }
}

#[test]
fn an_account_whose_available_balance_cannot_be_read_is_not_posted() {
    for available in [
        None,
        Some(json!(250.25)),
        Some(json!("x")),
        Some(json!(null)),
    ] {
        assert!(
            post(with(&["available_balance"], available.clone())).is_err(),
            "{available:?}"
        );
    }
}

#[test]
fn a_position_whose_size_cannot_be_read_is_not_left_out_of_the_post() {
    let built = post(with(&["positions", "0", "position"], Some(json!(1.5))));
    let positions = built.map(|report| report.positions.len());
    assert!(
        !matches!(positions, Ok(0)),
        "the account holds a position and the post names none"
    );
    for size in [None, Some(json!("1.5 SPY")), Some(json!(null))] {
        assert!(
            post(with(&["positions", "0", "position"], size.clone())).is_err(),
            "{size:?}"
        );
    }
}

#[test]
fn a_short_whose_sign_cannot_be_read_is_not_posted_as_a_long() {
    // The sign is missing: nothing says which side the position is on.
    let built = post(with(&["positions", "0", "sign"], None));
    assert!(
        built.is_err(),
        "a position with no side given was posted as a long of {}",
        built
            .map(|report| report.positions[0].size)
            .unwrap_or_default()
    );
    for sign in [json!("-1"), json!(0), json!(2), json!(-1.0)] {
        assert!(
            post(with(&["positions", "0", "sign"], Some(sign.clone()))).is_err(),
            "{sign}"
        );
    }
}

#[test]
fn a_position_naming_no_market_is_not_posted() {
    for market in [None, Some(json!("26"))] {
        assert!(
            post(with(&["positions", "0", "market_id"], market.clone())).is_err(),
            "{market:?}"
        );
    }
}

#[test]
fn a_flat_position_is_left_out_whatever_its_side() {
    // The exchange lists markets an account traded once, flat, with a side.
    let report = post(with(
        &["positions", "0"],
        Some(json!(
            {"market_id": 99, "position": "0.0000", "sign": 1}
        )),
    ))
    .unwrap();
    assert!(report.positions.is_empty());
}

#[test]
fn an_account_whose_positions_or_pool_shares_cannot_be_read_is_not_posted() {
    for field in ["positions", "shares", "index"] {
        assert!(post(with(&[field], None)).is_err(), "{field} missing");
        assert!(
            post(with(&[field], Some(json!("none")))).is_err(),
            "{field} unreadable"
        );
    }
}

#[test]
fn an_account_read_twice_is_not_posted_twice() {
    assert!(
        build(
            OnChain::default(),
            &[readable(), readable()],
            &[],
            &markets(),
            &[]
        )
        .is_err()
    );
}
