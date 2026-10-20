//! A vault's value is every share it has issued at the price of its last
//! valuation (the price every request is priced at), dated by that
//! valuation: a deposit priced since is in it, as the shares it made, the
//! day it is priced, not a day later at the next valuation. Dollars still in
//! escrow, waiting for a valuation, are not. A closed vault is worth the
//! cash its holders are paid from.
//!
//! Here: 150 shares at $1.10, the last valuation's gross $100 (before the
//! deposits it priced), $30 in escrow and $20 of cash.

mod stand_in;

use std::sync::Arc;

use alloy::primitives::U256;
use alloy::sol_types::SolCall;
use chain::bindings::vault::{IVault, VaultState};
use serde_json::{Value, json};
use stand_in::*;

const VALUED_AT: u64 = 1_789_990_000;

fn answering(state: VaultState) -> Answer {
    Arc::new(move |request: &Value| {
        if request["method"] != "eth_call" {
            return None;
        }
        let data = input(request);
        let word = |value: U256| Some(Ok(json!(format!("0x{value:064x}"))));
        if data == selector::<IVault::totalSupplyCall>() {
            word(U256::from(150u64) * U256::from(10u64.pow(18)))
        } else if data == selector::<IVault::sharePriceCall>() {
            word(U256::from(1_100_000u64))
        } else if data == selector::<IVault::totalValueCall>() {
            word(U256::from(100_000_000u64))
        } else if data == selector::<IVault::escrowCall>() {
            word(U256::from(30_000_000u64))
        } else if data == selector::<IVault::cashCall>() {
            word(U256::from(20_000_000u64))
        } else if data == selector::<IVault::stateCall>() {
            Some(Ok(json!(alloy::hex::encode_prefixed(
                IVault::stateCall::abi_encode_returns(&state)
            ))))
        } else if data == selector::<IVault::lastPricedCall>() {
            Some(Ok(json!(alloy::hex::encode_prefixed(
                IVault::lastPricedCall::abi_encode_returns(&IVault::lastPricedReturn {
                    priced: true,
                    index: U256::from(3),
                    time: VALUED_AT,
                })
            ))))
        } else {
            None
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_vault_s_value_is_every_share_at_the_last_valuation_s_price() {
    let indexer = an_indexer(answering(VaultState::Open), "value-open").await;
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await.clone();
    let vault = &snapshot.strategies[0].vault;
    assert!(
        (vault.value - 165.0).abs() < 1e-9,
        "150 shares at $1.10 are worth $165, shown {}",
        vault.value
    );
    assert_eq!(vault.valued_at, Some(indexer::read::moment(VALUED_AT)));
    assert!((snapshot.totals.vault_capital - 165.0).abs() < 1e-9);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_vault_is_worth_its_cash() {
    let indexer = an_indexer(answering(VaultState::Closed), "value-closed").await;
    indexer.refresh().await.unwrap();
    let snapshot = indexer.snapshot.read().await.clone();
    let vault = &snapshot.strategies[0].vault;
    assert!((vault.value - 20.0).abs() < 1e-9, "{}", vault.value);
}
