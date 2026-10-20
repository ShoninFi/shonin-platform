//! The clock and the market.
//!
//! On the local network a block is mined every second and moves the chain's
//! clock an hour, so a vault reaches the ninety live days it needs to be
//! ranked in about thirty-six minutes. On each block the stand-in venue is
//! given new prices and the funding that has accrued since the last one.
//!
//! The venue is kept able to pay what its accounts are owed ([`top_up`]). The venue is every trade's counterparty and the funding it
//! credits shorts comes from nobody, so without it the dollars its accounts
//! could withdraw drift above the dollars it holds; the operator mints the
//! difference to it, standing in for the longs who would have paid.
//!
//! The local walk is deterministic. A development chain that tells a
//! different story every time it starts is a poor place to debug one, and a
//! reproducible market means a demo can be run twice.

use std::collections::BTreeSet;
use std::time::Duration;

use alloy::primitives::{Address, I256, U256};
use alloy::rpc::types::Filter;
use alloy::sol_types::SolEvent;
use anyhow::{Context, Result};
use chain::bindings::erc20::IERC20;
use chain::bindings::usd::ILocalUsd;
use chain::bindings::venue::{Funding, ILocalVenue, Quote};
use chain::{Client, Deployments};

use crate::deploy::{PERP, SPOT, STARTING_PRICE, market_id};

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
}

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
    };

    /// The funding the basis mean pays, as a fraction of the price a day.
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
    #[cfg(test)]
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
        let mean = p.basis_mean;
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
        let posting = client.send_estimated(
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
/// doesn't. Returns what was minted.
async fn keep_solvent(
    client: &Client,
    deployments: &Deployments,
    operator: Address,
    book: &mut Book,
    paid: I256,
    ahead: I256,
) -> Result<U256> {
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
        return Ok(U256::ZERO);
    }
    client
        .send_estimated(
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
    Ok(mint)
}

fn to_signed(value: f64) -> I256 {
    let magnitude = I256::try_from(value.abs() as i128).unwrap_or(I256::ZERO);
    if value < 0.0 { -magnitude } else { magnitude }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // On a local chain -----------------------------------------------------

    struct Anvil(std::process::Child);

    impl Drop for Anvil {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A free port, never one of the development stack's own.
    fn free_port() -> u16 {
        loop {
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            if !(19000..=19200).contains(&port) {
                return port;
            }
        }
    }

    fn anvil() -> Option<(Anvil, u16)> {
        let port = free_port();
        let child = std::process::Command::new("anvil")
            .args([
                "--port",
                &port.to_string(),
                "--chain-id",
                "31337",
                "--silent",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let anvil = Anvil(child);
        let started = std::time::Instant::now();
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            if started.elapsed() > Duration::from_secs(20) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Some((anvil, port))
    }

    fn artifacts() -> Option<std::path::PathBuf> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm/out");
        path.join("LocalVenue.sol/LocalVenue.json")
            .exists()
            .then_some(path)
    }

    /// The local market's posts go out at their own estimate of gas (a node
    /// that signs for the operator would otherwise send a post it refuses):
    /// each lands, so both markets' prices and the perpetual's funding move
    /// block after block.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_local_market_moves_the_venue_s_prices_and_funding_block_after_block() {
        use chain::bindings::venue::ILocalVenue;
        let Some(artifacts) = artifacts() else {
            eprintln!("the contracts are not built: skipping");
            return;
        };
        let Some((_anvil, port)) = anvil() else {
            eprintln!("anvil is not installed: skipping");
            return;
        };
        let dir = std::env::temp_dir().join(format!("shonin-local-market-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("http://127.0.0.1:{port}");
        let config = chain::ChainConfig {
            label: "a local chain".to_string(),
            rpc: url.clone(),
            ws: None,
            dollar: "USDC".to_string(),
            native_usd: Some(2_500.0),
            public_rpc: url,
            logs_rpc: None,
            log_range: None,
            confirmations: Some(0),
        };
        let key: chain::ChainKey = "evm:31337".parse().unwrap();
        let network = chain::NetworkName::Local;
        let client = chain::connect_to(network, &key, &config).await.unwrap();
        let deployments = crate::deploy::ensure(
            &client,
            network,
            &key,
            &artifacts,
            &dir.join("evm-31337.json"),
            &[("base".to_string(), 31337)],
            0,
        )
        .await
        .unwrap();
        let market = |name: &'static str| {
            let client = client.clone();
            let venue = deployments.venue;
            async move {
                client
                    .read(
                        venue,
                        ILocalVenue::marketOfCall {
                            market: market_id(name),
                        },
                    )
                    .await
                    .unwrap()
            }
        };
        let before = market(PERP).await;
        let running = {
            let (client, deployments) = (client.clone(), deployments.clone());
            tokio::spawn(async move { run(&client, &deployments, 3_600).await })
        };
        let mut moved = 0;
        let mut last = before.fundingUpdatedAt;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let now = market(PERP).await;
            if now.fundingUpdatedAt > last {
                moved += 1;
                last = now.fundingUpdatedAt;
            }
            if moved >= 3 {
                break;
            }
        }
        running.abort();
        assert!(moved >= 3, "the perpetual's funding moved {moved} times");
        let perp = market(PERP).await;
        let spot = market(SPOT).await;
        assert_ne!(perp.mark, before.mark);
        assert_ne!(spot.mark, U256::from(STARTING_PRICE));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
