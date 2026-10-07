//! The clock and the market.
//!
//! On the local network a block is mined every second and moves the chain's
//! clock an hour, so a vault reaches the ninety live days it needs to be
//! ranked in about thirty-six minutes. On each block the venue is given new
//! prices and the funding that has accrued since the last one.
//!
//! On a testnet nothing moves the stand-in venue but its operator, so
//! [`operate`] plays it from the laptop in real time: every few minutes it
//! posts new prices and the funding accrued since the last post, from a tape
//! whose numbers look like a real market's ([`TapeParams::TESTNET`]).
//!
//! Either way the venue is kept able to pay what its accounts are owed
//! ([`top_up`]). The venue is every trade's counterparty and the funding it
//! credits shorts comes from nobody, so without it the dollars its accounts
//! could withdraw drift above the dollars it holds; the operator mints the
//! difference to it, standing in for the longs who would have paid.
//!
//! The local walk is deterministic. A development chain that tells a
//! different story every time it starts is a poor place to debug one, and a
//! reproducible market means a demo can be run twice. The testnet walk is
//! seeded from the chain's clock, so a restart doesn't replay the same day.

use std::collections::BTreeSet;
use std::time::Duration;

use alloy::primitives::{Address, I256, U256};
use alloy::rpc::types::Filter;
use alloy::sol_types::SolEvent;
use anyhow::{Context, Result, bail};
use chain::bindings::erc20::IERC20;
use chain::bindings::usd::ILocalUsd;
use chain::bindings::venue::{Funding, ILocalVenue, Quote};
use chain::{Client, Deployments, Role};

use crate::deploy::{PERP, SPOT, STARTING_PRICE, market_id};

/// A day, in seconds: funding rates are quoted per day.
const DAY: u64 = 86_400;

/// How a tape walks. Each volatility is a standard deviation over an hour.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TapeParams {
    /// How much the spot price moves in an hour, as a fraction.
    pub hourly_volatility: f64,
    /// The gap the perpetual keeps to the spot, which is what funding pays
    /// for, as a fraction of the spot.
    pub basis_mean: f64,
    pub basis_volatility: f64,
    /// How strongly the basis is pulled back towards its mean, per hour.
    pub basis_pull: f64,
    /// Half the spread the venue quotes, as a fraction of the price.
    pub half_spread: f64,
    /// Funding is settled this many times a day, so a basis of `b` is worth
    /// this many times `b` over a day.
    pub funding_per_day: f64,
    /// Spells when the market goes quiet and funding sinks towards nothing.
    pub lulls: Option<Lulls>,
}

/// Quiet spells: the basis is pulled towards `basis` instead of its mean for
/// `hours`, starting at random about once every `every_hours`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Lulls {
    pub every_hours: f64,
    pub hours: f64,
    pub basis: f64,
}

/// The testnet tape's funding over the long run, lulls included, as a
/// fraction of the price a day: 10% a year. A test holds the tape to it.
pub(crate) const TESTNET_MEAN_FUNDING: f64 = 0.10 / 365.0;

impl TapeParams {
    /// The local chain's market: a lively basis worth about 66% a year, so a
    /// carry shows its profit within minutes of a clock that runs an hour a
    /// second.
    pub(crate) const LOCAL: Self = Self {
        hourly_volatility: 0.004 / 3.0,
        basis_mean: 0.0006,
        basis_volatility: 0.0004 / 3.0,
        basis_pull: 0.06,
        half_spread: 0.0001,
        funding_per_day: 3.0,
        lulls: None,
    };

    /// A testnet's market, in real time, shaped like bitcoin's: the spot
    /// moves about 0.6% an hour, and the perpetual's funding averages about
    /// 10% a year (0.000274 of the price a day). Outside a lull it sits near
    /// 0.000295 a day, within about 0.00003 either way, and is pulled back
    /// with a half-life of two hours; about once every four days it goes
    /// quiet for eight hours and sinks to near nothing, which is when a
    /// carry's exit (0.00005 a day) and entry (0.0002 a day) are crossed.
    pub(crate) const TESTNET: Self = Self {
        hourly_volatility: 0.006,
        basis_mean: 0.000_295 / 3.0,
        // A stationary spread of 0.00003 a day in funding, for the pull
        // below: sigma * sqrt(2 * pull).
        basis_volatility: 0.000_03 / 3.0 * 0.836_660_026_5,
        basis_pull: 0.35,
        half_spread: 0.0001,
        funding_per_day: 3.0,
        lulls: Some(Lulls {
            every_hours: 96.0,
            hours: 8.0,
            basis: 0.000_01 / 3.0,
        }),
    };

    /// The funding the basis mean pays outside a lull, as a fraction of the
    /// price a day.
    pub(crate) fn funding_mean(&self) -> f64 {
        self.basis_mean * self.funding_per_day
    }
}

/// Deterministic noise: the same seed tells the same story every time.
pub(crate) struct Walk(u64);

impl Walk {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A number between -1 and 1, roughly bell-shaped, with a standard
    /// deviation of a third.
    fn wobble(&mut self) -> f64 {
        let sum = self.next() + self.next() + self.next();
        (sum / 3.0 - 0.5) * 2.0
    }

    /// Bell-shaped noise with a standard deviation of one, never beyond
    /// three.
    fn noise(&mut self) -> f64 {
        self.wobble() * 3.0
    }
}

/// The market's story, a step at a time: the spot price, the perpetual's,
/// and the funding that accrued over the step.
pub(crate) struct Tape {
    params: TapeParams,
    walk: Walk,
    spot: f64,
    basis: f64,
    /// What is left of the lull the market is in, in hours.
    lull_left: f64,
    /// How far [`Tape::step`] moves the market.
    hours_per_block: f64,
}

/// One step of the market.
pub(crate) struct Tick {
    pub spot: f64,
    pub perp: f64,
    /// What longs pay shorts per unit a day, and paid over this step.
    pub rate_per_day: f64,
    pub accrued: f64,
}

impl Tick {
    /// The funding rate as a fraction of the perpetual's price a day, which
    /// is how a strategy reads it.
    pub(crate) fn funding_fraction(&self) -> f64 {
        if self.perp <= 0.0 {
            0.0
        } else {
            self.rate_per_day / self.perp
        }
    }
}

impl Tape {
    /// The local chain's tape, a block of `seconds_per_block` at a step.
    pub(crate) fn new(seconds_per_block: u64) -> Self {
        let mut tape = Self::local();
        tape.hours_per_block = seconds_per_block as f64 / 3_600.0;
        tape
    }

    /// The local chain's tape, from the price the venue was listed at.
    pub(crate) fn local() -> Self {
        Self::with(
            TapeParams::LOCAL,
            0x5_0_1_1_u64.wrapping_mul(2_654_435_761),
            STARTING_PRICE as f64,
            TapeParams::LOCAL.basis_mean,
        )
    }

    pub(crate) fn with(params: TapeParams, seed: u64, spot: f64, basis: f64) -> Self {
        Self {
            params,
            walk: Walk::new(seed),
            spot,
            basis,
            lull_left: 0.0,
            hours_per_block: 1.0,
        }
    }

    /// Move the market on by a block.
    pub(crate) fn step(&mut self) -> Tick {
        self.step_by(self.hours_per_block)
    }

    /// Move the market on by `hours`.
    pub(crate) fn step_by(&mut self, hours: f64) -> Tick {
        let p = self.params;
        let mut mean = p.basis_mean;
        if let Some(lulls) = p.lulls {
            if self.lull_left <= 0.0 && self.walk.next() < hours / lulls.every_hours {
                self.lull_left = lulls.hours;
            }
            if self.lull_left > 0.0 {
                mean = lulls.basis;
                self.lull_left -= hours;
            }
        }
        self.spot *= 1.0 + p.hourly_volatility * self.walk.noise() * hours.sqrt();
        self.basis += (mean - self.basis) * (p.basis_pull * hours).min(1.0)
            + p.basis_volatility * self.walk.noise() * hours.sqrt();
        let perp = self.spot * (1.0 + self.basis);
        // Longs pay shorts while the perpetual trades above the spot, which is
        // the whole reason a basis strategy exists.
        let rate_per_day = perp * self.basis * p.funding_per_day;
        Tick {
            spot: self.spot,
            perp,
            rate_per_day,
            accrued: rate_per_day * hours / 24.0,
        }
    }

    pub(crate) fn half_spread(&self, price: f64) -> f64 {
        (price * self.params.half_spread).max(1.0)
    }

    fn quote(&self, market: &str, price: f64) -> Quote {
        let mark = U256::from(price.max(1.0) as u128);
        let half = U256::from(self.half_spread(price) as u128);
        Quote {
            market: market_id(market),
            mark,
            oracle: mark,
            bid: mark.saturating_sub(half),
            ask: mark + half,
        }
    }
}

/// Half the spread the local venue quotes.
pub(crate) fn half_spread(price: f64) -> f64 {
    (price * TapeParams::LOCAL.half_spread).max(1.0)
}

/// How often, in blocks, the local market makes sure the venue can pay.
const LOCAL_SOLVENCY_EVERY: u64 = 12;

pub async fn run(client: &Client, deployments: &Deployments, seconds_per_block: u64) -> Result<()> {
    set_clock(client, seconds_per_block).await?;

    let operator = deployments.roles.operator;
    let venue = deployments.venue;
    let mut tape = Tape::new(seconds_per_block);
    let mut book = Book::new(deployments.deployed_block);

    tracing::info!(
        seconds_per_block,
        "the clock is running; the venue is quoting"
    );

    let mut block = 0u64;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let tick = tape.step();
        block += 1;

        if (block - 1).is_multiple_of(LOCAL_SOLVENCY_EVERY) {
            let paid = to_signed(tick.accrued);
            let ahead = to_signed(tick.accrued * (2 * LOCAL_SOLVENCY_EVERY) as f64);
            if let Err(error) =
                keep_solvent(client, deployments, operator, &mut book, paid, ahead).await
            {
                tracing::warn!(error = %format!("{error:#}"), "the venue's dollars weren't checked");
            }
        }

        // One transaction a block: the chain mines on a timer, and waiting for
        // four receipts would leave the market four blocks behind its own
        // clock.
        let posting = client.send(
            operator,
            venue,
            ILocalVenue::postCall {
                quotes: vec![tape.quote(SPOT, tick.spot), tape.quote(PERP, tick.perp)],
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

// The testnet operator ------------------------------------------------------

/// The longest stretch one post accrues funding for. An operator stopped for
/// longer starts again without paying out days at a rate nobody saw quoted.
const LONGEST_ACCRUAL: u64 = DAY;

/// What one unit of a perpetual pays over `elapsed` seconds at `rate_per_day`
/// (the market's funding rate, in the asset's units per unit a day), rounded
/// towards zero.
pub(crate) fn paid_per_unit(rate_per_day: I256, elapsed: u64) -> I256 {
    rate_per_day * I256::try_from(elapsed).unwrap_or(I256::ZERO) / I256::try_from(DAY).unwrap()
}

/// Run the stand-in venue's market on a testnet, in real time: a post of
/// both markets' prices and the perpetual's funding every `every`, signed by
/// the operator, and the venue kept able to pay before each.
///
/// It starts from the venue's prices as they stand, so it can be stopped and
/// started again. Each post accrues the funding of the rate that was quoted
/// from the last accrual until now, by the chain's clock, so a slow block or
/// a missed post is paid for exactly once.
pub async fn operate(
    client: &Client,
    deployments: &Deployments,
    every: Duration,
    params: TapeParams,
) -> Result<()> {
    let operator = client.account(Role::Operator)?;
    if operator != deployments.roles.operator {
        bail!(
            "this key's operator is {operator}, and the venue's is {}: the mnemonic is not the one this deployment was made with",
            deployments.roles.operator
        );
    }
    let venue = deployments.venue;
    let spot = read_market(client, venue, SPOT).await?;
    let perp = read_market(client, venue, PERP).await?;
    let spot_price = to_f64(spot.mark);
    let basis = starting_basis(&params, spot_price, to_f64(perp.mark), perp.fundingRate);
    let now = client.block_timestamp().await?;
    let mut tape = Tape::with(params, now, spot_price, basis);
    let mut book = Book::new(deployments.deployed_block);
    let mut gas = GasTally::default();
    let posts_a_day = DAY as f64 / every.as_secs_f64();

    tracing::info!(
        chain = client.chain_id,
        minutes = every.as_secs_f64() / 60.0,
        spot = spot_price / 1e6,
        funding_a_year = basis * params.funding_per_day * 365.0,
        "the market operator is running: a post each interval, from the venue's prices as they stand"
    );

    let mut timer = tokio::time::interval(every);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = tokio::time::Instant::now();
    loop {
        timer.tick().await;
        let hours = last.elapsed().as_secs_f64().max(1.0) / 3_600.0;
        last = tokio::time::Instant::now();
        let tick = tape.step_by(hours);
        match post_once(client, deployments, operator, &mut book, &tape, &tick).await {
            Ok(posted) => {
                gas.add(&posted);
                tracing::info!(
                    chain = client.chain_id,
                    spot = format!("{:.2}", tick.spot / 1e6),
                    perp = format!("{:.2}", tick.perp / 1e6),
                    funding_a_day = format!("{:.6}", tick.funding_fraction()),
                    funding_a_year = format!("{:.2}%", tick.funding_fraction() * 36_500.0),
                    paid_per_unit = %posted.paid,
                    minted = %posted.minted,
                    gas = posted.post_gas,
                    gas_a_day = format!("{:.0}", gas.average() * posts_a_day),
                    coin_a_day = format!("{:.8}", gas.average_coin() * posts_a_day),
                    "posted"
                );
            }
            Err(error) => {
                tracing::warn!(chain = client.chain_id, error = %format!("{error:#}"), "the post did not land; the next one accrues for this stretch too");
            }
        }
    }
}

/// Where a restarted operator's basis starts: the gap between the venue's
/// two prices, or failing that the funding rate it quotes, or the mean.
fn starting_basis(params: &TapeParams, spot: f64, perp: f64, rate: I256) -> f64 {
    let from_gap = if spot > 0.0 && perp > 0.0 && perp != spot {
        Some(perp / spot - 1.0)
    } else {
        None
    };
    let from_rate =
        (perp > 0.0 && !rate.is_zero()).then(|| i256_to_f64(rate) / perp / params.funding_per_day);
    from_gap
        .or(from_rate)
        .filter(|basis| basis.is_finite() && basis.abs() < 0.01)
        .unwrap_or(params.basis_mean)
}

/// What one post did.
struct Posted {
    paid: I256,
    minted: U256,
    post_gas: u64,
    /// Every transaction's gas in the coin, the mint's too.
    coin: f64,
}

async fn post_once(
    client: &Client,
    deployments: &Deployments,
    operator: Address,
    book: &mut Book,
    tape: &Tape,
    tick: &Tick,
) -> Result<Posted> {
    let venue = deployments.venue;
    let perp = read_market(client, venue, PERP).await?;
    let now = client.block_timestamp().await?;
    let elapsed = now
        .saturating_sub(perp.fundingUpdatedAt)
        .min(LONGEST_ACCRUAL);
    let paid = paid_per_unit(perp.fundingRate, elapsed);
    // A day ahead at the new rate, so a mint is needed about once a day
    // rather than at every post.
    let ahead = paid_per_unit(to_signed(tick.rate_per_day), DAY);
    let (minted, mint_coin) =
        keep_solvent(client, deployments, operator, book, paid, ahead).await?;
    let receipt = client
        .send(
            operator,
            venue,
            ILocalVenue::postCall {
                quotes: vec![tape.quote(SPOT, tick.spot), tape.quote(PERP, tick.perp)],
                funding: vec![Funding {
                    market: market_id(PERP),
                    ratePerDay: to_signed(tick.rate_per_day),
                    paidPerUnit: paid,
                }],
            },
        )
        .await
        .context("posting the venue's prices")?;
    Ok(Posted {
        paid,
        minted,
        post_gas: receipt.gas_used,
        coin: coin_spent(&receipt) + mint_coin,
    })
}

/// What a landed transaction cost in the chain's coin.
fn coin_spent(receipt: &alloy::rpc::types::TransactionReceipt) -> f64 {
    receipt.gas_used as f64 * receipt.effective_gas_price as f64 / 1e18
}

/// The operator's gas so far, for the per-day figure each post logs.
#[derive(Default)]
struct GasTally {
    posts: u64,
    gas: u64,
    coin: f64,
}

impl GasTally {
    fn add(&mut self, posted: &Posted) {
        self.posts += 1;
        self.gas += posted.post_gas;
        self.coin += posted.coin;
    }

    fn average(&self) -> f64 {
        self.gas as f64 / self.posts.max(1) as f64
    }

    fn average_coin(&self) -> f64 {
        self.coin / self.posts.max(1) as f64
    }
}

async fn read_market(
    client: &Client,
    venue: Address,
    name: &str,
) -> Result<chain::bindings::venue::Market> {
    client
        .read(
            venue,
            ILocalVenue::marketOfCall {
                market: market_id(name),
            },
        )
        .await
        .with_context(|| format!("reading the venue's {name} market"))
}

// Solvency ------------------------------------------------------------------

/// Every account the venue has ever taken dollars from, found from its
/// `Deposited` events and kept up to date a block range at a time. An
/// account can only come to hold anything by depositing first.
pub(crate) struct Book {
    masters: BTreeSet<Address>,
    next_block: u64,
}

impl Book {
    pub(crate) fn new(from_block: u64) -> Self {
        Self {
            masters: BTreeSet::new(),
            next_block: from_block,
        }
    }

    async fn refresh(&mut self, client: &Client, venue: Address) -> Result<()> {
        let head = client.block_number().await?;
        if head < self.next_block {
            return Ok(());
        }
        let filter = Filter::new()
            .address(venue)
            .event_signature(ILocalVenue::Deposited::SIGNATURE_HASH)
            .from_block(self.next_block)
            .to_block(head);
        for log in client.logs(&filter).await.context("the venue's deposits")? {
            if let Some(topic) = log.topics().get(1) {
                self.masters.insert(Address::from_word(*topic));
            }
        }
        self.next_block = head + 1;
        Ok(())
    }
}

/// One account at the venue, as solvency needs it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Owed {
    /// What it could take out now, at mark, its unsettled funding counted.
    pub value: U256,
    /// Its perpetual position, 18 decimals, short below zero.
    pub perp_size: I256,
}

/// What a funding accrual of `paid` per unit adds to what the venue owes:
/// the side being paid gains `|size * paid|`. The side paying loses as much,
/// but an account can't owe the venue past its own value, so that is not
/// counted against it.
pub(crate) fn gained(accounts: &[Owed], paid: I256) -> U256 {
    let unit = I256::try_from(1_000_000_000_000_000_000u128).unwrap();
    accounts
        .iter()
        .filter(|account| {
            (account.perp_size.is_negative() && paid.is_positive())
                || (account.perp_size.is_positive() && paid.is_negative())
        })
        .map(|account| (account.perp_size * paid / unit).unsigned_abs())
        .fold(U256::ZERO, |total, one| total + one)
}

/// How much to mint to the venue so it holds what its accounts are owed
/// after this post's accrual of `paid` per unit: nothing while its `balance`
/// covers that; otherwise enough to cover it with a cushion of `ahead` per
/// unit's accrual and a hundredth of what is owed besides, so the next mint
/// is a while away.
pub(crate) fn top_up(balance: U256, accounts: &[Owed], paid: I256, ahead: I256) -> U256 {
    let owed = accounts
        .iter()
        .fold(U256::ZERO, |total, account| total + account.value);
    let needed = owed + gained(accounts, paid);
    if balance >= needed {
        return U256::ZERO;
    }
    needed + gained(accounts, ahead) + owed / U256::from(100u64) - balance
}

/// Make sure the venue holds what its accounts are owed once `paid` per unit
/// has accrued, minting the development dollar to it (anyone may) when it
/// doesn't. Returns what was minted and what the mint cost in the coin.
async fn keep_solvent(
    client: &Client,
    deployments: &Deployments,
    operator: Address,
    book: &mut Book,
    paid: I256,
    ahead: I256,
) -> Result<(U256, f64)> {
    let venue = deployments.venue;
    book.refresh(client, venue).await?;
    let mut accounts = Vec::with_capacity(book.masters.len());
    for master in &book.masters {
        let value = client
            .read(venue, ILocalVenue::accountValueCall { master: *master })
            .await?;
        let position = client
            .read(
                venue,
                ILocalVenue::positionOfCall {
                    master: *master,
                    market: market_id(PERP),
                },
            )
            .await?;
        accounts.push(Owed {
            value,
            perp_size: position.size,
        });
    }
    let balance = client
        .read(deployments.usd, IERC20::balanceOfCall { account: venue })
        .await?;
    let mint = top_up(balance, &accounts, paid, ahead);
    if mint.is_zero() {
        return Ok((U256::ZERO, 0.0));
    }
    let receipt = client
        .send(
            operator,
            deployments.usd,
            ILocalUsd::mintCall {
                to: venue,
                amount: mint,
            },
        )
        .await
        .context("minting the venue what its accounts are owed")?;
    tracing::info!(
        chain = client.chain_id,
        minted = %mint,
        accounts = accounts.len(),
        "the venue was short of what its accounts are owed: minted it the difference, for the longs who pay the funding"
    );
    Ok((mint, coin_spent(&receipt)))
}

fn to_signed(value: f64) -> I256 {
    let magnitude = I256::try_from(value.abs() as i128).unwrap_or(I256::ZERO);
    if value < 0.0 { -magnitude } else { magnitude }
}

fn to_f64(value: U256) -> f64 {
    value.to_string().parse().unwrap_or(0.0)
}

fn i256_to_f64(value: I256) -> f64 {
    value.to_string().parse().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ten simulated years at a post every ten minutes.
    fn run_testnet(seed: u64) -> Vec<f64> {
        let hours = 10.0 / 60.0;
        let steps = (10.0 * 365.0 * 24.0 / hours) as usize;
        let mut tape = Tape::with(
            TapeParams::TESTNET,
            seed,
            STARTING_PRICE as f64,
            TapeParams::TESTNET.basis_mean,
        );
        (0..steps)
            .map(|_| tape.step_by(hours).funding_fraction())
            .collect()
    }

    #[test]
    fn the_testnet_tape_averages_about_ten_percent_a_year() {
        for seed in [1, 7, 1_000_003] {
            let funding = run_testnet(seed);
            let mean = funding.iter().sum::<f64>() / funding.len() as f64;
            let a_year = mean * 365.0;
            assert!(
                (0.09..=0.11).contains(&a_year),
                "seed {seed}: {a_year} a year"
            );
            // Its spread: a standard deviation of a few points a year, lulls
            // included.
            let variance =
                funding.iter().map(|f| (f - mean).powi(2)).sum::<f64>() / funding.len() as f64;
            let spread = variance.sqrt() * 365.0;
            assert!((0.005..=0.04).contains(&spread), "seed {seed}: {spread}");
        }
    }

    /// A carry exits below 0.00005 a day and enters again above 0.0002. The
    /// market should make it do so now and then, never every hour.
    #[test]
    fn the_testnet_tape_rarely_crosses_below_the_exit() {
        const EXIT: f64 = 0.000_05;
        const ENTRY: f64 = 0.000_2;
        for seed in [1, 7, 1_000_003] {
            let funding = run_testnet(seed);
            let below = funding.iter().filter(|f| **f < EXIT).count() as f64;
            let share = below / funding.len() as f64;
            assert!((0.002..=0.05).contains(&share), "seed {seed}: {share}");
            // Exits that follow an entry, as the carry would see them.
            let (mut exits, mut entered) = (0, true);
            for f in &funding {
                if entered && *f < EXIT {
                    exits += 1;
                    entered = false;
                } else if *f >= ENTRY {
                    entered = true;
                }
            }
            let a_day = exits as f64 / (10.0 * 365.0);
            assert!((0.05..=0.5).contains(&a_day), "seed {seed}: {a_day} a day");
        }
    }

    #[test]
    fn the_local_tape_is_the_one_it_always_was() {
        let mut tape = Tape::new(3_600);
        let first = tape.step();
        // The local chain's story: about 66% a year, every run alike.
        assert!((first.funding_fraction() * 365.0 - 0.66).abs() < 0.5);
        let again = Tape::new(3_600).step();
        assert_eq!(first.spot, again.spot);
        assert_eq!(first.perp, again.perp);
    }

    #[test]
    fn paid_per_unit_is_the_rate_over_the_time_elapsed() {
        // $18 a unit a day (0.0003 of $60,000), for ten minutes.
        let rate = I256::try_from(18_000_000i64).unwrap();
        assert_eq!(
            paid_per_unit(rate, 600),
            I256::try_from(125_000i64).unwrap()
        );
        assert_eq!(paid_per_unit(rate, DAY), rate);
        assert_eq!(paid_per_unit(rate, 0), I256::ZERO);
        // Negative funding pays the longs, and rounds towards zero.
        assert_eq!(paid_per_unit(-rate, 1), -I256::try_from(208i64).unwrap());
    }

    fn size(units: i64) -> I256 {
        I256::try_from(units).unwrap() * I256::try_from(1_000_000_000_000_000_000u128).unwrap()
    }

    /// After a mint, the venue holds every account's value once the accrual
    /// has been credited to the side being paid.
    #[test]
    fn the_venue_is_kept_solvent() {
        let short = Owed {
            value: U256::from(20_000_000_000u64),
            perp_size: size(-2),
        };
        let long = Owed {
            value: U256::from(5_000_000_000u64),
            perp_size: size(1),
        };
        let flat = Owed {
            value: U256::from(1_000_000_000u64),
            perp_size: I256::ZERO,
        };
        let accounts = [short, long, flat];
        let paid = I256::try_from(125_000i64).unwrap();
        let ahead = I256::try_from(18_000_000i64).unwrap();
        // The short is credited 2 x 0.125; the long's loss isn't counted.
        let after = U256::from(26_000_000_000u64 + 250_000);

        // Short by a dollar: minted enough, with the cushion.
        let balance = U256::from(25_999_000_000u64);
        let mint = top_up(balance, &accounts, paid, ahead);
        assert!(balance + mint >= after);
        assert_eq!(
            balance + mint,
            after + U256::from(36_000_000u64) + U256::from(260_000_000u64)
        );

        // Enough already: nothing.
        assert_eq!(top_up(after, &accounts, paid, ahead), U256::ZERO);
        // A unit short of the accrual alone: a mint.
        assert!(!top_up(after - U256::from(1u64), &accounts, paid, ahead).is_zero());

        // Negative funding pays the long.
        let paid = -paid;
        let mint = top_up(U256::ZERO, &accounts, paid, I256::ZERO);
        assert!(mint >= U256::from(26_000_000_000u64 + 125_000));
    }

    #[test]
    fn a_restarted_operator_starts_from_the_venues_prices() {
        let params = TapeParams::TESTNET;
        let basis = starting_basis(&params, 60_000e6, 60_006e6, I256::ZERO);
        assert!((basis - 0.0001).abs() < 1e-12);
        // Equal prices: from the quoted rate, 0.0003 of the price a day.
        let rate = I256::try_from(18_000_000i64).unwrap();
        let basis = starting_basis(&params, 60_000e6, 60_000e6, rate);
        assert!((basis - 0.0001).abs() < 1e-12);
        // Nothing to go on: the mean.
        assert_eq!(
            starting_basis(&params, 60_000e6, 60_000e6, I256::ZERO),
            params.basis_mean
        );
    }
}
