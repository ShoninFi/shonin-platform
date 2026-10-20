//! The vault never trades: its home station is the venue account's
//! master, and the executor places every order with `master: station`, so
//! the venue's `Filled` names the station in its first topic. A vault's
//! trades are its station's fills: on its page, in its record's count, and
//! in the platform's total.
//!
//! Here: a node holding one fill of the vault's station.

mod stand_in;

use std::sync::Arc;

use alloy::primitives::{Address, B256, U256};
use chain::bindings::vault::IVault;
use chain::bindings::venue::ILocalVenue;
use serde_json::{Value, json};
use stand_in::*;

const STATION: Address = Address::repeat_byte(0x5a);
const VENUE: Address = Address::with_last_byte(1);

/// The one fill on the chain: the station's.
fn the_fill() -> Value {
    a_log(
        VENUE,
        ILocalVenue::Filled {
            master: STATION,
            market: B256::with_last_byte(7),
            by: Address::repeat_byte(0xee),
            isBuy: true,
            reduceOnly: false,
            size: U256::from(10u64.pow(18)),
            price: U256::from(100_000_000u64),
            notional: U256::from(100_000_000u64),
        },
        1,
        0,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_vault_s_trades_are_its_station_s_fills() {
    let indexer = an_indexer(
        Arc::new(|request: &Value| match request["method"].as_str() {
            Some("eth_getLogs") => Some(Ok(json!(matching(request, &[the_fill()])))),
            Some("eth_call") if input(request) == selector::<IVault::homeStationCall>() => {
                Some(Ok(json!(format!("0x{:0>64}", alloy::hex::encode(STATION)))))
            }
            _ => None,
        }),
        "trades",
    )
    .await;
    indexer.refresh().await.unwrap();

    let snapshot = indexer.snapshot.read().await.clone();
    let strategy = snapshot.strategies.first().expect("the vault is read");
    assert_eq!(
        strategy.vault.station.to_lowercase(),
        format!("{STATION:?}").to_lowercase(),
        "the stand-in names the station"
    );
    assert!(
        strategy.trades.len() == 1 && snapshot.totals.trades == 1,
        "the station filled one order; the page shows {} trades, the totals {}",
        strategy.trades.len(),
        snapshot.totals.trades
    );
}
