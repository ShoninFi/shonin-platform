//! What a day of running cost, said once a day, charged to the vault's tank.
//!
//! The statement is the author's bill and nobody else's: a tank is the
//! author's balance, never depositors' money. It says what the day cost at the
//! platform's published prices, and the vault's contract believes it only up
//! to the allowances the author declared, so a wrong statement can never
//! charge more than the author agreed to.
//!
//! What a day costs is the capacity the strategy held, for the time it held
//! it. A strategy reserves its declared allowances for as long as it runs —
//! the fuel rate it is topped up to, the memory it may grow into — whether or
//! not it spends them, so that is what it pays for.
//!
//! The time is measured on the wall clock, not the chain's. A development
//! chain's clock runs fast, and a machine costs what it costs however quickly
//! the chain believes the days are passing. The chain's clock decides only
//! which day a statement is filed for.

use std::time::{Duration, Instant};

use alloy::primitives::{Address, U256};
use anyhow::{Context, Result};
use chain::bindings::vault::IVault;
use chain::{Client, KeyClient};
use shared::{DeclaredAllowances, Prices};

use crate::scale::ASSET_DECIMALS;

/// A chain day, which on a development chain passes rather faster than a real
/// one.
const DAY: u64 = 86_400;

/// Meter one vault and say what it cost, once a day.
pub async fn run(chain: Client, key: KeyClient, vault: Address, allowances: DeclaredAllowances) {
    // Nothing is charged for the time before this instance took the vault on.
    let mut since = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;
        match state(&chain, &key, vault, allowances, since).await {
            Ok(Some(until)) => since = until,
            Ok(None) => {}
            Err(error) => tracing::debug!(%vault, %error, "could not state the day's usage"),
        }
    }
}

/// Charge the day just gone, if it has not been charged. Returns the moment
/// the statement covered up to, so the next one measures from there.
async fn state(
    chain: &Client,
    key: &KeyClient,
    vault: Address,
    allowances: DeclaredAllowances,
    since: Instant,
) -> Result<Option<Instant>> {
    let now = chain.block_timestamp().await?;
    let today = now / DAY;
    if today == 0 {
        return Ok(None);
    }
    let stating = today - 1;

    let tank = chain.read(vault, IVault::tankCall {}).await?;
    if stating <= tank.lastChargedDay {
        return Ok(None);
    }

    // How long the machine actually ran for what is being stated.
    let ends = Instant::now();
    let ran = ends.duration_since(since).as_secs();
    let amount = cost(ran, allowances);

    let digest = chain
        .read(
            vault,
            IVault::usageDigestCall {
                day: stating,
                amount,
            },
        )
        .await?;
    let signature = key.sign(digest).context("signing the statement")?;

    key.send(
        vault,
        IVault::submitUsageCall {
            day: stating,
            amount,
            signature,
        },
    )
    .await
    .context("submitting the statement")?;
    tracing::info!(%vault, day = stating, seconds = ran, "the day's usage is stated");
    Ok(Some(ends))
}

/// What holding those allowances for that long costs, at the published prices.
fn cost(seconds: u64, allowances: DeclaredAllowances) -> U256 {
    let day = Prices::published().daily_cap(&allowances);
    let dollars = day * (seconds.min(DAY) as f64 / DAY as f64);
    let units = (dollars * 10f64.powi(ASSET_DECIMALS as i32))
        .round()
        .max(0.0);
    U256::from(units as u128)
}
