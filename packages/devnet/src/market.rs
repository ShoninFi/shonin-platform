//! The clock and the market.
//!
//! A block is mined every second and moves the chain's clock an hour, so a
//! vault reaches the ninety live days it needs to be ranked in about
//! thirty-six minutes. On each block the venue is given new prices and the
//! funding that has accrued since the last one.
//!
//! The walk is deterministic. A development chain that tells a different story
//! every time it starts is a poor place to debug one, and a reproducible
//! market means a demo can be run twice.

use std::time::Duration;

use alloy::primitives::{I256, U256};
use anyhow::{Context, Result};
use chain::bindings::venue::{Funding, ILocalVenue, Quote};
use chain::{Client, Deployments};

use crate::deploy::{PERP, SPOT, STARTING_PRICE, market_id};

/// How much the spot price moves in an hour, as a fraction.
const HOURLY_VOLATILITY: f64 = 0.004;
/// The gap the perpetual keeps to the spot, which is what funding pays for.
const BASIS_MEAN: f64 = 0.0006;
const BASIS_VOLATILITY: f64 = 0.0004;
/// How strongly the basis is pulled back towards its mean each hour.
const BASIS_PULL: f64 = 0.06;
/// Half the spread the venue quotes, as a fraction of the price.
const HALF_SPREAD: f64 = 0.0001;
/// Funding is settled three times a day, so a basis of `b` is worth about
/// three times `b` over a day.
const FUNDING_PER_DAY: f64 = 3.0;

/// Deterministic noise: the same chain tells the same story every time.
pub(crate) struct Walk(u64);

impl Walk {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    /// A number between -1 and 1, roughly bell-shaped.
    fn wobble(&mut self) -> f64 {
        let mut sum = 0.0;
        for _ in 0..3 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            sum += (self.0 >> 11) as f64 / (1u64 << 53) as f64;
        }
        (sum / 3.0 - 0.5) * 2.0
    }
}

/// The market's story, a block at a time: the spot price, the perpetual's,
/// and the funding that accrued over the block.
pub(crate) struct Tape {
    walk: Walk,
    spot: f64,
    basis: f64,
    hours_per_block: f64,
}

/// One block of the market.
pub(crate) struct Tick {
    pub spot: f64,
    pub perp: f64,
    /// What longs pay shorts per unit a day, and paid over this block.
    pub rate_per_day: f64,
    pub accrued: f64,
}

impl Tape {
    pub(crate) fn new(seconds_per_block: u64) -> Self {
        Self {
            walk: Walk::new(0x5_0_1_1_u64.wrapping_mul(2_654_435_761)),
            spot: STARTING_PRICE as f64,
            basis: BASIS_MEAN,
            hours_per_block: seconds_per_block as f64 / 3_600.0,
        }
    }

    pub(crate) fn step(&mut self) -> Tick {
        let hours = self.hours_per_block;
        self.spot *= 1.0 + HOURLY_VOLATILITY * self.walk.wobble() * hours.sqrt();
        self.basis += (BASIS_MEAN - self.basis) * BASIS_PULL * hours
            + BASIS_VOLATILITY * self.walk.wobble() * hours.sqrt();
        let perp = self.spot * (1.0 + self.basis);
        // Longs pay shorts while the perpetual trades above the spot, which is
        // the whole reason a basis strategy exists.
        let rate_per_day = perp * self.basis * FUNDING_PER_DAY;
        Tick {
            spot: self.spot,
            perp,
            rate_per_day,
            accrued: rate_per_day * hours / 24.0,
        }
    }
}

/// Half the spread the venue quotes, as a fraction of the price.
pub(crate) fn half_spread(price: f64) -> f64 {
    (price * HALF_SPREAD).max(1.0)
}

pub async fn run(client: &Client, deployments: &Deployments, seconds_per_block: u64) -> Result<()> {
    set_clock(client, seconds_per_block).await?;

    let operator = deployments.roles.operator;
    let venue = deployments.venue;
    let mut tape = Tape::new(seconds_per_block);

    tracing::info!(
        seconds_per_block,
        "the clock is running; the venue is quoting"
    );

    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let tick = tape.step();

        // One transaction a block: the chain mines on a timer, and waiting for
        // four receipts would leave the market four blocks behind its own
        // clock.
        let posting = client.send(
            operator,
            venue,
            ILocalVenue::postCall {
                quotes: vec![quote(SPOT, tick.spot), quote(PERP, tick.perp)],
                funding: vec![Funding {
                    market: market_id(PERP),
                    ratePerDay: to_signed(tick.rate_per_day),
                    paidPerUnit: to_signed(tick.accrued),
                }],
            },
        );
        if let Err(error) = posting.await {
            tracing::warn!(%error, "the venue would not take the quote");
        }
    }
}

/// Each block moves the chain's clock by a fixed stretch, so time passes
/// faster than it does here.
///
/// A vault's valuation counts only within seconds of a ten-second boundary,
/// so the next block is put on one first; with a stretch that is a multiple
/// of ten, every block after it lands on one too, and a valuation recorded in
/// any block is inside its window.
async fn set_clock(client: &Client, seconds_per_block: u64) -> Result<()> {
    const SLOT: u64 = 10;
    let now = client.block_timestamp().await?;
    let aligned = now - now % SLOT + SLOT;
    client
        .raw_request("evm_setNextBlockTimestamp", serde_json::json!([aligned]))
        .await
        .context("putting the next block on a slot boundary")?;
    let seconds_per_block = seconds_per_block.div_ceil(SLOT) * SLOT;
    client
        .raw_request(
            "anvil_setBlockTimestampInterval",
            serde_json::json!([seconds_per_block]),
        )
        .await
        .context("setting how far a block moves the clock")?;
    Ok(())
}

fn quote(market: &str, price: f64) -> Quote {
    let mark = U256::from(price.max(1.0) as u128);
    let half = U256::from(half_spread(price) as u128);
    Quote {
        market: market_id(market),
        mark,
        oracle: mark,
        bid: mark.saturating_sub(half),
        ask: mark + half,
    }
}

fn to_signed(value: f64) -> I256 {
    let magnitude = I256::try_from(value.abs() as i128).unwrap_or(I256::ZERO);
    if value < 0.0 { -magnitude } else { magnitude }
}
