//! What running a vault's agent has cost: one running total, signed with the
//! vault's key and handed to whoever collects it.
//!
//! The total is the author's bill and nobody else's: a tank is the author's
//! balance, never depositors' money. The vault believes it only up to the
//! allowances the author set, at the published prices, for the time that has
//! passed, so a wrong total can never charge more than the author agreed to.
//!
//! What it costs is the capacity the agent held, for the time it held it: the
//! allowances in force, whether or not it spent them. The time is measured on
//! the wall clock, not the chain's, because a development chain's clock runs
//! fast and a machine costs what it costs however quickly the chain believes
//! the days are passing.
//!
//! When the unpaid part of the total reaches what the tank holds, the agent is
//! stopped, and it starts again on a top-up: the contract has no "dry" state,
//! and stopping is this executor's job.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, Bytes, U256};
use anyhow::{Context, Result};
use chain::bindings::vault::IVault;
use chain::{Client, KeyClient};
use serde::Serialize;
use shared::{Allowances, Prices};

use crate::scale::ASSET_DECIMALS;

/// A signed running total, as the keeper collects it.
#[derive(Clone, Debug, Serialize)]
pub struct Signed {
    /// In the asset's smallest unit.
    pub total: String,
    pub as_of: u64,
    pub signature: Bytes,
}

/// The latest total for each vault this executor serves, by chain id and
/// vault.
pub type Totals = Arc<Mutex<HashMap<(u64, Address), Signed>>>;

/// Meter one vault, keep its signed total current, and say when its tank is
/// dry.
pub async fn run(
    chain: Client,
    key: KeyClient,
    vault: Address,
    totals: Totals,
    dry: Arc<AtomicBool>,
) {
    // The total carries on from what the vault has already collected, so it
    // never goes backwards across a restart; nothing is counted for the time
    // before this instance took the vault on.
    let mut base = None;
    let mut since = Instant::now();
    let mut accrued = 0.0f64;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let now = Instant::now();
        let seconds = now.duration_since(since).as_secs_f64();
        since = now;
        match tick(&chain, &key, vault, &mut base, &mut accrued, seconds).await {
            Ok((signed, is_dry)) => {
                if let Ok(mut totals) = totals.lock() {
                    totals.insert((chain.chain_id, vault), signed);
                }
                if dry.swap(is_dry, Ordering::SeqCst) != is_dry {
                    if is_dry {
                        tracing::warn!(%vault, "the tank is dry: the agent stops until it is topped up");
                    } else {
                        tracing::info!(%vault, "the tank has money again");
                    }
                }
            }
            Err(error) => tracing::debug!(%vault, %error, "could not meter the vault"),
        }
    }
}

async fn tick(
    chain: &Client,
    key: &KeyClient,
    vault: Address,
    base: &mut Option<U256>,
    accrued: &mut f64,
    seconds: f64,
) -> Result<(Signed, bool)> {
    let tank = chain.read(vault, IVault::tankCall {}).await?;
    let base = *base.get_or_insert(tank.usageSettled);
    // The allowances in force now, so a change the author makes is charged
    // from the moment it applies.
    let allowances = shared_allowances(&chain.read(vault, IVault::allowancesCall {}).await?);
    *accrued += cost(seconds, &allowances);
    let total = base + U256::from(accrued.round().max(0.0) as u128);
    let as_of = chain.block_timestamp().await?;

    let digest = chain
        .read(vault, IVault::usageDigestCall { total, asOf: as_of })
        .await?;
    let signature = key.sign(digest).context("signing the total")?;

    let unpaid = total.saturating_sub(tank.usageSettled);
    Ok((
        Signed {
            total: total.to_string(),
            as_of,
            signature,
        },
        unpaid >= tank.balance,
    ))
}

/// What holding these allowances for this long costs at the published prices,
/// in the asset's smallest unit.
fn cost(seconds: f64, allowances: &Allowances) -> f64 {
    let day = Prices::published().daily_cap(allowances);
    day * (seconds / 86_400.0) * 10f64.powi(ASSET_DECIMALS as i32)
}

/// The vault's allowances as the platform's own type.
pub fn shared_allowances(on_chain: &chain::bindings::prices::Allowances) -> Allowances {
    Allowances {
        fuel_per_sec: on_chain.fuelPerSec,
        memory_mib: on_chain.memoryMib,
        requests_per_min: on_chain.requestsPerMin,
        bytes_per_day: on_chain.bytesPerDay,
        streams: on_chain.streams,
        inference_ms_per_min: on_chain.inferenceMsPerMin,
        gas_per_day: on_chain.gasPerDay as f64 / 1_000_000.0,
    }
}
