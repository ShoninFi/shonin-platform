//! Whether a vault pays its own way, read from the chain alone.
//!
//! A demonstration agent runs with its tank on auto-fill: the author's fee
//! shares, minted at each payday and redeemed into the tank by the keeper's
//! daily call, have to cover the tank's bills. This reads everything that
//! decides it (the vault's value and share price, the tank, the allowances
//! and what a day of them costs, the fees owed and paid, the author's
//! shares) and the gas the vault's key and the keeper spend on it, and
//! projects a day: the bill against the author's fee at the funding rate the
//! venue quotes, and the deposit at which the two meet.

use std::collections::BTreeMap;

use alloy::primitives::{Address, B256, U256};
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use anyhow::{Context, Result};
use chain::bindings::prices::IPrices;
use chain::bindings::station::IStation;
use chain::bindings::vault::IVault;
use chain::bindings::venue::ILocalVenue;
use chain::{Client, Deployments};
use serde::Serialize;

use crate::deploy::{PERP, SPOT, market_id};

/// The share of a vault `funding-basis` keeps short, when it holds nothing
/// to measure: it aims to have 80% of the vault in its two legs, half of it
/// in the short.
pub const CARRY_SHARE: f64 = 0.4;

/// Dollars carry six decimals on every chain the platform settles on.
const DOLLAR: f64 = 1e6;
const BPS: f64 = 10_000.0;

/// What a day of the vault looks like, worked out from a few figures.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Projection {
    /// What the vault is worth, in dollars.
    pub vault_value: f64,
    /// The short's notional as a share of the vault.
    pub carry_share: f64,
    /// Funding, as a fraction of the perpetual's price a day.
    pub funding_per_day: f64,
    /// What the short earns a day, and the holding fee a day.
    pub funding_income: f64,
    pub holding_fee: f64,
    /// The vault's gain a day, after the holding fee and before the profit
    /// fee, and the author's tenth of it.
    pub gain: f64,
    pub author_fee: f64,
    /// The tank's bill a day: the allowances' cost, and the agent's gas.
    pub bill: f64,
    /// The author's fee less the bill: above zero, the vault pays its way.
    pub surplus: f64,
    /// The vault size at which the author's fee meets the bill; none when a
    /// vault of any size loses money at this funding rate.
    pub break_even_deposit: Option<f64>,
}

impl Projection {
    /// A day at `funding_per_day` on `carry_share` of `vault_value`, after a
    /// holding fee of `holding_bps` a year, with the author taking
    /// `author_bps` of the gain and paying `bill` a day.
    pub fn new(
        vault_value: f64,
        carry_share: f64,
        funding_per_day: f64,
        holding_bps: f64,
        author_bps: f64,
        bill: f64,
    ) -> Self {
        // The gain on each dollar of the vault a day.
        let per_dollar = carry_share * funding_per_day - holding_bps / BPS / 365.0;
        let funding_income = vault_value * carry_share * funding_per_day;
        let holding_fee = vault_value * holding_bps / BPS / 365.0;
        let gain = funding_income - holding_fee;
        let author_fee = (gain * author_bps / BPS).max(0.0);
        let earns = per_dollar * author_bps / BPS;
        Self {
            vault_value,
            carry_share,
            funding_per_day,
            funding_income,
            holding_fee,
            gain,
            author_fee,
            bill,
            surplus: author_fee - bill,
            break_even_deposit: (earns > 0.0).then(|| bill / earns),
        }
    }
}

/// What the report is asked for.
pub struct Options {
    pub vault: Address,
    /// How far back the gas and the keeper's sends are counted.
    pub hours: f64,
    /// The chain's coin in dollars at a mainnet-like price; the chain's own
    /// guess (ETH or HYPE) when not given.
    pub coin_usd: Option<f64>,
    /// The coin's price the tank is billed at on this chain (zero on the
    /// testnets).
    pub billed_coin_usd: f64,
    pub carry_share: Option<f64>,
    /// The mean funding to project at besides the current one, as a
    /// fraction of the price a day.
    pub mean_funding: f64,
    pub json: bool,
}

/// A mainnet-like price for the chain's coin: HYPE on Hyperliquid's chains,
/// ETH elsewhere.
pub fn mainnet_coin_usd(chain_id: u64) -> f64 {
    match chain_id {
        998 | 999 => 25.0,
        _ => 2_500.0,
    }
}

#[derive(Serialize)]
struct Report {
    chain_id: u64,
    vault: Address,
    state: String,
    vault_value: f64,
    share_price: f64,
    total_shares: f64,
    cash: f64,
    high_water_mark: f64,
    last_payday: u64,
    fees_bps: FeesBps,
    tank: TankReport,
    allowances: serde_json::Value,
    daily_cost: f64,
    daily_cost_without_gas: f64,
    fee_owed: f64,
    author_fee_owed: f64,
    author: Address,
    author_shares: f64,
    author_shares_worth: f64,
    venue: VenueReport,
    gas: GasReport,
    projections: Vec<Labelled>,
}

#[derive(Serialize)]
struct FeesBps {
    holding: u16,
    author: u16,
    platform: u16,
    transaction: u16,
}

#[derive(Serialize)]
struct TankReport {
    balance: f64,
    keep_at: f64,
    usage_settled: f64,
    usage_as_of: u64,
    fill_pending: bool,
}

#[derive(Serialize)]
struct VenueReport {
    station: Address,
    account_value: f64,
    spot_units: f64,
    perp_units: f64,
    short_notional: f64,
    funding_per_day: f64,
    funding_a_year: f64,
}

#[derive(Serialize)]
struct GasReport {
    window_hours: f64,
    from_block: u64,
    to_block: u64,
    coin_usd_mainnet_like: f64,
    coin_usd_billed: f64,
    key: Option<Address>,
    key_nonce: Option<u64>,
    key_balance: Option<f64>,
    key_sends: u64,
    key_gas: u64,
    key_coin: f64,
    key_coin_per_day: f64,
    keeper_sends: u64,
    keeper_sends_per_hour: f64,
    keeper_gas: u64,
    keeper_coin: f64,
    keeper_coin_per_day: f64,
    keeper_sends_by_hour: BTreeMap<u64, u64>,
    other_sends: u64,
}

#[derive(Serialize)]
struct Labelled {
    label: String,
    projection: Projection,
}

/// A vault's state by name, in the order the contract declares them.
fn state_name(state: u8) -> &'static str {
    [
        "none",
        "activating",
        "open",
        "winding down",
        "frozen",
        "killed",
        "closed",
        "cancelled",
    ]
    .get(usize::from(state))
    .copied()
    .unwrap_or("unknown")
}

fn dollars(value: U256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0) / DOLLAR
}

fn units(value: alloy::primitives::I256) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0) / 1e18
}

pub async fn report(client: &Client, deployments: &Deployments, options: &Options) -> Result<()> {
    let vault = options.vault;
    let state = client.read(vault, IVault::stateCall {}).await?;
    let share_price = client.read(vault, IVault::sharePriceCall {}).await?;
    let value = client.read(vault, IVault::totalValueCall {}).await?;
    let supply = client.read(vault, IVault::totalSupplyCall {}).await?;
    let cash = client.read(vault, IVault::cashCall {}).await?;
    let mark = client.read(vault, IVault::markCall {}).await?;
    let last_payday = client.read(vault, IVault::lastPaymentCall {}).await?;
    let fees = client.read(vault, IVault::feesCall {}).await?;
    let tank = client.read(vault, IVault::tankCall {}).await?;
    let allowances = client.read(vault, IVault::allowancesCall {}).await?;
    let author = client.read(vault, IVault::authorCall {}).await?;
    let author_shares = client
        .read(vault, IVault::balanceOfCall { owner: author })
        .await?;
    let station = client.read(vault, IVault::homeStationCall {}).await?;
    let priced = client.read(vault, IVault::lastPricedCall {}).await?;
    let fee_owed = if priced.priced {
        client
            .read(
                vault,
                IVault::valuationAtCall {
                    index: priced.index,
                },
            )
            .await?
            .fee
    } else {
        U256::ZERO
    };
    let profit_bps = u64::from(fees.authorBps) + u64::from(fees.platformBps);
    let author_fee_owed = if profit_bps == 0 {
        U256::ZERO
    } else {
        fee_owed * U256::from(fees.authorBps) / U256::from(profit_bps)
    };

    let daily_cost = client
        .read(
            deployments.prices,
            IPrices::dailyCostCall {
                allowances: allowances.clone(),
            },
        )
        .await?;
    let mut without_gas = allowances.clone();
    without_gas.gasPerDay = 0;
    let daily_cost_without_gas = client
        .read(
            deployments.prices,
            IPrices::dailyCostCall {
                allowances: without_gas,
            },
        )
        .await?;

    // At the venue.
    let venue = deployments.venue;
    let account_value = client
        .read(venue, ILocalVenue::accountValueCall { master: station })
        .await?;
    let position = |name: &str| ILocalVenue::positionOfCall {
        master: station,
        market: market_id(name),
    };
    let spot_units = units(client.read(venue, position(SPOT)).await?.size);
    let perp_units = units(client.read(venue, position(PERP)).await?.size);
    let perp = client
        .read(
            venue,
            ILocalVenue::marketOfCall {
                market: market_id(PERP),
            },
        )
        .await?;
    let perp_mark = dollars(perp.mark);
    let rate: f64 = perp.fundingRate.to_string().parse::<f64>().unwrap_or(0.0) / DOLLAR;
    let funding_per_day = if perp_mark > 0.0 {
        rate / perp_mark
    } else {
        0.0
    };
    let short_notional = (-perp_units).max(0.0) * perp_mark;

    let gas = gas_spent(client, deployments, vault, station, options).await?;

    // The projections.
    let vault_value = dollars(value);
    let measured =
        (vault_value > 0.0 && short_notional > 0.0).then(|| short_notional / vault_value);
    let (carry_share, carry_label) = match (options.carry_share, measured) {
        (Some(share), _) => (share, "as asked"),
        (None, Some(share)) => (share, "as held"),
        (None, None) => (CARRY_SHARE, "the agent's aim; it holds none yet"),
    };
    let usage = dollars(daily_cost_without_gas);
    let billed = usage + gas.key_coin_per_day * options.billed_coin_usd;
    let mainnet_like = usage + gas.key_coin_per_day * gas.coin_usd_mainnet_like;
    let project = |funding: f64, bill: f64| {
        Projection::new(
            vault_value,
            carry_share,
            funding,
            f64::from(fees.holdingBps),
            f64::from(fees.authorBps),
            bill,
        )
    };
    let projections = vec![
        Labelled {
            label: format!("the funding quoted now, the gas billed as here (carry {carry_label})"),
            projection: project(funding_per_day, billed),
        },
        Labelled {
            label: "the funding quoted now, the agent's gas at a mainnet-like price".into(),
            projection: project(funding_per_day, mainnet_like),
        },
        Labelled {
            label: "the market's mean funding, the gas billed as here".into(),
            projection: project(options.mean_funding, billed),
        },
        Labelled {
            label: "the market's mean funding, the agent's gas at a mainnet-like price".into(),
            projection: project(options.mean_funding, mainnet_like),
        },
    ];

    let report = Report {
        chain_id: client.chain_id,
        vault,
        state: state_name(state as u8).to_string(),
        vault_value,
        share_price: dollars(share_price),
        total_shares: supply.to_string().parse::<f64>().unwrap_or(0.0) / 1e18,
        cash: dollars(cash),
        high_water_mark: dollars(mark),
        last_payday,
        fees_bps: FeesBps {
            holding: fees.holdingBps,
            author: fees.authorBps,
            platform: fees.platformBps,
            transaction: fees.transactionBps,
        },
        tank: TankReport {
            balance: dollars(tank.balance),
            keep_at: dollars(tank.keepAt),
            usage_settled: dollars(tank.usageSettled),
            usage_as_of: tank.usageAsOf,
            fill_pending: tank.fillPending,
        },
        allowances: serde_json::json!({
            "fuel_per_sec": allowances.fuelPerSec,
            "memory_mib": allowances.memoryMib,
            "requests_per_min": allowances.requestsPerMin,
            "bytes_per_day": allowances.bytesPerDay,
            "streams": allowances.streams,
            "inference_ms_per_min": allowances.inferenceMsPerMin,
            "gas_per_day": allowances.gasPerDay as f64 / DOLLAR,
        }),
        daily_cost: dollars(daily_cost),
        daily_cost_without_gas: usage,
        fee_owed: dollars(fee_owed),
        author_fee_owed: dollars(author_fee_owed),
        author,
        author_shares: author_shares.to_string().parse::<f64>().unwrap_or(0.0) / 1e18,
        author_shares_worth: dollars(
            author_shares * share_price / U256::from(10u64).pow(U256::from(18u64)),
        ),
        venue: VenueReport {
            station,
            account_value: dollars(account_value),
            spot_units,
            perp_units,
            short_notional,
            funding_per_day,
            funding_a_year: funding_per_day * 365.0,
        },
        gas,
        projections,
    };

    if options.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_report(&report);
    }
    Ok(())
}

fn print_report(r: &Report) {
    println!("Vault {:?} on chain {} ({})", r.vault, r.chain_id, r.state);
    println!(
        "  value ${:.2}, share price ${:.6}, {:.4} shares, cash ${:.2}",
        r.vault_value, r.share_price, r.total_shares, r.cash
    );
    println!(
        "  high-water mark ${:.6} a share; last payday at {}; fees (bps): holding {} a year, author {}, platform {}, transaction {}",
        r.high_water_mark,
        r.last_payday,
        r.fees_bps.holding,
        r.fees_bps.author,
        r.fees_bps.platform,
        r.fees_bps.transaction
    );
    println!(
        "  profit fee owed at the last priced valuation ${:.4}, the author's part ${:.4}",
        r.fee_owed, r.author_fee_owed
    );
    println!(
        "  the author {:?} holds {:.6} shares, worth ${:.4}",
        r.author, r.author_shares, r.author_shares_worth
    );
    println!(
        "Tank: ${:.4}, kept at ${:.4}{}; usage collected ${:.4} as of {}",
        r.tank.balance,
        r.tank.keep_at,
        if r.tank.fill_pending {
            " (a fill is waiting)"
        } else {
            ""
        },
        r.tank.usage_settled,
        r.tank.usage_as_of
    );
    println!(
        "  allowances {}; a day of them ${:.4} (${:.4} without the gas allowance)",
        r.allowances, r.daily_cost, r.daily_cost_without_gas
    );
    println!(
        "Venue: station {:?} worth ${:.2}; {:.6} spot, {:.6} perp (short ${:.2}); funding {:.6} a day ({:.2}% a year)",
        r.venue.station,
        r.venue.account_value,
        r.venue.spot_units,
        r.venue.perp_units,
        r.venue.short_notional,
        r.venue.funding_per_day,
        r.venue.funding_a_year * 100.0
    );
    let g = &r.gas;
    println!(
        "Gas over the last {:.1} h (blocks {}..={}), the coin at ${} (billed here at ${}):",
        g.window_hours, g.from_block, g.to_block, g.coin_usd_mainnet_like, g.coin_usd_billed
    );
    match g.key {
        Some(key) => println!(
            "  the vault's key {key:?}: {} sends ({} gas, {:.8} coin, {:.8} a day = ${:.4} a day at the mainnet-like price); {} sends in its life; holds {:.6}",
            g.key_sends,
            g.key_gas,
            g.key_coin,
            g.key_coin_per_day,
            g.key_coin_per_day * g.coin_usd_mainnet_like,
            g.key_nonce.map_or("?".into(), |n| n.to_string()),
            g.key_balance.unwrap_or(0.0)
        ),
        None => println!("  no key approved at the station yet"),
    }
    println!(
        "  the keeper: {} sends to it, {:.2} an hour ({} gas, {:.8} coin, {:.8} a day = ${:.4} a day at the mainnet-like price; the platform's, not billed)",
        g.keeper_sends,
        g.keeper_sends_per_hour,
        g.keeper_gas,
        g.keeper_coin,
        g.keeper_coin_per_day,
        g.keeper_coin_per_day * g.coin_usd_mainnet_like
    );
    if !g.keeper_sends_by_hour.is_empty() {
        let hours: Vec<String> = g
            .keeper_sends_by_hour
            .iter()
            .map(|(hour, count)| format!("{hour}:{count}"))
            .collect();
        println!(
            "  the keeper's sends by hour (unix hour:count): {}",
            hours.join(" ")
        );
    }
    if g.other_sends > 0 {
        println!("  {} sends by others", g.other_sends);
    }
    println!("A day, projected:");
    for labelled in &r.projections {
        let p = &labelled.projection;
        println!("  at {}:", labelled.label);
        println!(
            "    funding {:.6} a day on {:.0}% of ${:.2}: ${:.4} earned, ${:.4} holding fee, ${:.4} gain",
            p.funding_per_day,
            p.carry_share * 100.0,
            p.vault_value,
            p.funding_income,
            p.holding_fee,
            p.gain
        );
        println!(
            "    the author's fee ${:.4} against a bill of ${:.4}: {} ${:.4}; break-even at {}",
            p.author_fee,
            p.bill,
            if p.surplus >= 0.0 {
                "pays its way by"
            } else {
                "short by"
            },
            p.surplus.abs(),
            p.break_even_deposit
                .map_or("no size at this rate".into(), |v| format!("${v:.0}"))
        );
    }
}

/// The gas the vault's key and the keeper spent on the vault over the
/// window, from the receipts of every transaction that touched the vault,
/// its home station or the station's account at the venue. A send that
/// reverted leaves no event and is not counted.
async fn gas_spent(
    client: &Client,
    deployments: &Deployments,
    vault: Address,
    station: Address,
    options: &Options,
) -> Result<GasReport> {
    let provider = client.provider();
    let (head, now) = client.head_and_time().await?;
    let since = now.saturating_sub((options.hours * 3_600.0) as u64);
    let from_block = block_at(client, deployments.deployed_block, head, since).await?;

    let key = match client
        .read(station, IStation::specCall {})
        .await?
        .drivers
        .first()
    {
        Some(driver) => {
            let key = client
                .read(station, IStation::approvedKeyCall { driver: *driver })
                .await?;
            (!key.is_zero()).then_some(key)
        }
        None => None,
    };

    let mut hashes: Vec<B256> = Vec::new();
    let ours = Filter::new()
        .address(vec![vault, station])
        .from_block(from_block)
        .to_block(head);
    let at_venue = Filter::new()
        .address(deployments.venue)
        .topic1(station.into_word())
        .from_block(from_block)
        .to_block(head);
    for filter in [ours, at_venue] {
        for log in client.logs(&filter).await.context("the vault's events")? {
            if let Some(hash) = log.transaction_hash
                && !hashes.contains(&hash)
            {
                hashes.push(hash);
            }
        }
    }

    let keeper = deployments.roles.keeper;
    let mut report = GasReport {
        window_hours: options.hours,
        from_block,
        to_block: head,
        coin_usd_mainnet_like: options
            .coin_usd
            .unwrap_or_else(|| mainnet_coin_usd(client.chain_id)),
        coin_usd_billed: options.billed_coin_usd,
        key,
        key_nonce: None,
        key_balance: None,
        key_sends: 0,
        key_gas: 0,
        key_coin: 0.0,
        key_coin_per_day: 0.0,
        keeper_sends: 0,
        keeper_sends_per_hour: 0.0,
        keeper_gas: 0,
        keeper_coin: 0.0,
        keeper_coin_per_day: 0.0,
        keeper_sends_by_hour: BTreeMap::new(),
        other_sends: 0,
    };
    for hash in hashes {
        let Some(receipt) = provider
            .get_transaction_receipt(hash)
            .await
            .context("a receipt")?
        else {
            continue;
        };
        let coin = receipt.gas_used as f64 * receipt.effective_gas_price as f64 / 1e18;
        if Some(receipt.from) == key {
            report.key_sends += 1;
            report.key_gas += receipt.gas_used;
            report.key_coin += coin;
        } else if receipt.from == keeper {
            report.keeper_sends += 1;
            report.keeper_gas += receipt.gas_used;
            report.keeper_coin += coin;
            if let Some(block) = receipt.block_number
                && let Ok(time) = client.block_timestamp_at(block).await
            {
                *report.keeper_sends_by_hour.entry(time / 3_600).or_default() += 1;
            }
        } else {
            report.other_sends += 1;
        }
    }
    let days = options.hours / 24.0;
    if days > 0.0 {
        report.key_coin_per_day = report.key_coin / days;
        report.keeper_coin_per_day = report.keeper_coin / days;
        report.keeper_sends_per_hour = report.keeper_sends as f64 / options.hours;
    }
    if let Some(key) = key {
        report.key_nonce = provider.get_transaction_count(key).await.ok();
        report.key_balance = client
            .balance(key)
            .await
            .ok()
            .map(|wei| wei.to_string().parse::<f64>().unwrap_or(0.0) / 1e18);
    }
    Ok(report)
}

/// The first block at or after `time`, between `low` and `high`.
async fn block_at(client: &Client, mut low: u64, mut high: u64, time: u64) -> Result<u64> {
    if client.block_timestamp_at(low).await? >= time {
        return Ok(low);
    }
    while low + 1 < high {
        let middle = low + (high - low) / 2;
        if client.block_timestamp_at(middle).await? < time {
            low = middle;
        } else {
            high = middle;
        }
    }
    Ok(high)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6 * b.abs().max(1.0)
    }

    /// `funding-basis` at $25,000 with the modest allowances ($0.0585 a day,
    /// gas billed at nothing), 40% of the vault short at 10% a year, a 1%
    /// holding fee and the author's 10%.
    #[test]
    fn a_funding_basis_vault_of_25000_pays_its_way() {
        let p = Projection::new(25_000.0, 0.4, 0.10 / 365.0, 100.0, 1_000.0, 0.0585);
        assert!(close(p.funding_income, 1_000.0 / 365.0));
        assert!(close(p.holding_fee, 250.0 / 365.0));
        assert!(close(p.gain, 750.0 / 365.0));
        assert!(close(p.author_fee, 75.0 / 365.0));
        assert!(close(p.surplus, 75.0 / 365.0 - 0.0585));
        // 0.0585 / (0.1 * (0.4 * 0.1 - 0.01) / 365): $7,117.50.
        assert!(close(p.break_even_deposit.unwrap(), 0.0585 * 365.0 / 0.003));
        assert!((p.break_even_deposit.unwrap() - 7_117.5).abs() < 0.01);
    }

    #[test]
    fn gas_raises_the_break_even_in_proportion() {
        let p = Projection::new(25_000.0, 0.4, 0.10 / 365.0, 100.0, 1_000.0, 0.1585);
        assert!(close(p.break_even_deposit.unwrap(), 0.1585 * 365.0 / 0.003));
    }

    /// Funding that doesn't beat the holding fee leaves the author nothing,
    /// at any size.
    #[test]
    fn funding_below_the_holding_fee_never_breaks_even() {
        let p = Projection::new(25_000.0, 0.4, 0.02 / 365.0, 100.0, 1_000.0, 0.0585);
        assert!(p.gain < 0.0);
        assert_eq!(p.author_fee, 0.0);
        assert!(p.surplus < 0.0);
        assert_eq!(p.break_even_deposit, None);
    }
}
