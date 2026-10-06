//! The attested read: what an executor or unwinder image reads from
//! Lighter's API about a station's accounts and posts on the board, where
//! the driver values the station from it.
//!
//! Everything is posted in the venue's own integer units (sizes in base
//! units, prices in ticks), exactly as the API gives them, so the contract
//! does the arithmetic and nothing is rounded twice.

use alloy::primitives::{Address, U256};
use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use crate::markets::{Markets, integer, pow10};
use crate::payload::Decimal;

alloy::sol! {
    struct LighterPosition {
        uint48 account;
        uint16 market;
        int64 size;
    }
    struct LighterPrice {
        uint16 market;
        uint32 mark;
        uint32 index;
        uint32 bid;
        uint32 ask;
    }
    struct LighterShares {
        uint48 account;
        uint48 pool;
        uint64 shares;
        uint256 value;
    }
    struct LighterReport {
        uint64 asOf;
        uint48[] accounts;
        uint256 equity;
        uint256 pending;
        uint256 arrived;
        uint256 sent;
        uint256[] available;
        LighterPosition[] positions;
        LighterPrice[] prices;
        LighterShares[] pools;
    }

    #[sol(rpc)]
    interface ILighterBoard {
        function setPubKey(bytes calldata pubKey) external;
        function post(address station, LighterReport calldata report) external;
        function pubKeyOf(address key) external view returns (bytes memory);
        function reportedAt(address station) external view returns (uint64);
    }
}

/// One market's prices as read, in ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prices {
    pub market: u16,
    pub mark: u32,
    pub index: u32,
    pub bid: u32,
    pub ask: u32,
}

/// A decimal the API writes as text, and only as text: a figure that is
/// missing, written as a number, or not a decimal is an error, never zero.
fn decimal(value: &Value, what: &str) -> Result<Decimal> {
    let text = value
        .as_str()
        .ok_or_else(|| anyhow!("{what} is not given as text: {value}"))?;
    text.trim()
        .parse()
        .map_err(|_| anyhow!("{what} can't be read: {text:?}"))
}

/// Dollars (USDG, six decimals) from the API's decimal text, rounded down.
/// Less than nothing (an account under water) is nothing: what the station
/// can lose there is what it put in.
fn dollars(value: &Value, what: &str) -> Result<U256> {
    let amount = decimal(value, what)?;
    if amount.is_sign_negative() {
        return Ok(U256::ZERO);
    }
    Ok(U256::from(integer((amount * pow10(6)).floor())?))
}

/// What the report reads on-chain, all at one block: its time, which the
/// report is stamped with (never the machine's clock, which may run ahead of
/// the chain's and have the board refuse it), the station's pending balance
/// at the exchange, and the station's own counters (every dollar that had
/// reached it from outside its calls, and every dollar it had put into the
/// exchange), which tell the driver what moved after the reading.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OnChain {
    pub as_of: u64,
    pub pending: U256,
    pub arrived: U256,
    pub sent: U256,
}

/// The report for `accounts` (each the API's account object), with what was
/// read on-chain beside them, each held market's prices, and the account of
/// every public pool the accounts hold shares in (`pools`, each the API's
/// account object for the pool, which says what the pool is worth and how
/// many shares it has).
///
/// Every figure posted is read from the answer exactly; one that is missing,
/// written in another type, or can't be read stops the whole post (nothing
/// is ever posted as zero, flat or long for want of a figure).
pub fn build(
    on_chain: OnChain,
    accounts: &[Value],
    prices: &[Prices],
    markets: &Markets,
    pools: &[Value],
) -> Result<LighterReport> {
    let mut report = LighterReport {
        asOf: on_chain.as_of,
        accounts: Vec::new(),
        equity: U256::ZERO,
        pending: on_chain.pending,
        arrived: on_chain.arrived,
        sent: on_chain.sent,
        available: Vec::new(),
        positions: Vec::new(),
        prices: prices
            .iter()
            .map(|p| LighterPrice {
                market: p.market,
                mark: p.mark,
                index: p.index,
                bid: p.bid,
                ask: p.ask,
            })
            .collect(),
        pools: Vec::new(),
    };
    for account in accounts {
        let index = account["index"]
            .as_u64()
            .ok_or_else(|| anyhow!("an account without an index: {}", account["index"]))?;
        if report
            .accounts
            .iter()
            .any(|known| known.to::<u64>() == index)
        {
            bail!("account {index} is read twice");
        }
        let index = alloy::primitives::Uint::<48, 1>::try_from(index)
            .map_err(|_| anyhow!("account index {index} is out of range"))?;
        report.accounts.push(index);
        report.equity += dollars(
            &account["total_asset_value"],
            &format!("account {index}'s worth"),
        )?;
        report.available.push(dollars(
            &account["available_balance"],
            &format!("account {index}'s available balance"),
        )?);
        let positions = account["positions"]
            .as_array()
            .ok_or_else(|| anyhow!("account {index}'s positions can't be read"))?;
        for position in positions {
            let market_id = position["market_id"]
                .as_u64()
                .ok_or_else(|| anyhow!("a position of account {index} names no market"))?;
            let size = decimal(
                &position["position"],
                &format!("account {index}'s position in market {market_id}"),
            )?;
            if size.is_zero() {
                continue;
            }
            if size.is_sign_negative() {
                bail!("account {index}'s position in market {market_id} is written signed: {size}");
            }
            let side: i64 = match position["sign"].as_i64() {
                Some(1) => 1,
                Some(-1) => -1,
                _ => bail!(
                    "account {index}'s position in market {market_id} has no side: {}",
                    position["sign"]
                ),
            };
            let market = u16::try_from(market_id)
                .ok()
                .and_then(|id| markets.by_index(id))
                .ok_or_else(|| anyhow!("a position in unknown market {market_id}"))?;
            let units = i64::try_from(integer((size * pow10(market.size_decimals)).floor())?)
                .map_err(|_| {
                    anyhow!("account {index}'s position in market {market_id} is out of range")
                })?;
            if units == 0 {
                bail!(
                    "account {index}'s position in market {market_id} ({size}) is finer than the market's sizes"
                );
            }
            report.positions.push(LighterPosition {
                account: index,
                market: market.index,
                size: side * units,
            });
        }
        let shares = account["shares"]
            .as_array()
            .ok_or_else(|| anyhow!("account {index}'s pool shares can't be read"))?;
        for share in shares {
            let pool = share["public_pool_index"]
                .as_u64()
                .ok_or_else(|| anyhow!("a pool share of account {index} names no pool"))?;
            let amount = share["shares_amount"]
                .as_u64()
                .ok_or_else(|| anyhow!("account {index}'s shares in pool {pool} can't be read"))?;
            if amount == 0 {
                continue;
            }
            report.pools.push(LighterShares {
                account: index,
                pool: alloy::primitives::Uint::<48, 1>::try_from(pool)
                    .map_err(|_| anyhow!("pool index {pool} is out of range"))?,
                shares: amount,
                value: pool_value(pools, pool, amount)?,
            });
        }
    }
    Ok(report)
}

/// What `shares` of public pool `pool` are worth, in dollar units, rounded
/// down: the pool's worth times the shares over all its shares, from the
/// pool's own account.
fn pool_value(pools: &[Value], pool: u64, shares: u64) -> Result<U256> {
    let account = pools
        .iter()
        .find(|account| account["index"].as_u64() == Some(pool))
        .ok_or_else(|| anyhow!("pool {pool}'s account wasn't read"))?;
    let worth = dollars(
        &account["total_asset_value"],
        &format!("pool {pool}'s worth"),
    )?;
    let total = account["pool_info"]["total_shares"]
        .as_u64()
        .filter(|total| *total > 0)
        .ok_or_else(|| anyhow!("pool {pool}'s shares can't be read"))?;
    if shares > total {
        bail!("pool {pool} has {total} shares, fewer than the {shares} held in it");
    }
    Ok(worth * U256::from(shares) / U256::from(total))
}

/// The public pools `accounts` hold shares in, by index, each once.
pub fn pools_held(accounts: &[Value]) -> Vec<u64> {
    let mut pools: Vec<u64> = accounts
        .iter()
        .flat_map(|account| account["shares"].as_array().into_iter().flatten())
        .filter(|share| share["shares_amount"].as_u64() != Some(0))
        .filter_map(|share| share["public_pool_index"].as_u64())
        .collect();
    pools.sort_unstable();
    pools.dedup();
    pools
}

/// A market's prices from its details (mark, index) and its book's best
/// orders.
pub fn prices(market: u16, details: &Value, book: &Value, markets: &Markets) -> Result<Prices> {
    let m = markets
        .by_index(market)
        .ok_or_else(|| anyhow!("unknown market {market}"))?;
    let detail = &details["order_book_details"][0];
    if detail["market_id"].as_u64() != Some(u64::from(market)) {
        bail!(
            "asked for market {market}'s details, Lighter answered with market {}",
            detail["market_id"]
        );
    }
    let ticks = |value: &Value, what: &str| -> Result<u32> {
        let price = decimal(value, &format!("market {market}'s {what}"))?;
        if price.is_sign_negative() || price.is_zero() {
            bail!("market {market}'s {what} is not positive: {price}");
        }
        let ticks = integer((price * pow10(m.price_decimals)).round(0))?;
        u32::try_from(ticks).map_err(|_| anyhow!("market {market}'s {what} is out of range"))
    };
    // A side of the book with no orders on it has no best price: zero, which
    // the driver reads as a market outside its band. A side that isn't in
    // the answer at all can't be read.
    let best = |side: &str| -> Result<u32> {
        let orders = book[side]
            .as_array()
            .ok_or_else(|| anyhow!("market {market}'s {side} can't be read"))?;
        orders
            .first()
            .map(|order| ticks(&order["price"], &format!("best of the {side}")))
            .unwrap_or(Ok(0))
    };
    Ok(Prices {
        market,
        mark: ticks(&detail["mark_price"], "mark price")?,
        index: ticks(&detail["index_price"], "index price")?,
        bid: best("bids")?,
        ask: best("asks")?,
    })
}

/// Which address a report is about, for logs.
pub fn describe(station: Address, report: &LighterReport) -> String {
    format!(
        "{station}: {} accounts, equity {}, {} positions",
        report.accounts.len(),
        report.equity,
        report.positions.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::sol_types::SolValue;

    fn markets() -> Markets {
        Markets::from_order_books(&serde_json::json!({"order_books": [
            {"symbol": "SPY", "market_id": 26, "market_type": "perp", "status": "active",
             "supported_size_decimals": 4, "supported_price_decimals": 2, "min_base_amount": "0.0100"}
        ]}))
        .unwrap()
    }

    #[test]
    fn a_short_position_and_the_accounts_worth_are_read_in_the_venues_units() {
        let account = serde_json::json!({
            "index": 40518, "total_asset_value": "1000.5", "available_balance": "250.25",
            "positions": [{"market_id": 26, "position": "1.5000", "sign": -1}],
            "shares": []
        });
        let on_chain = OnChain {
            as_of: 5,
            pending: U256::from(7u8),
            arrived: U256::from(11u8),
            sent: U256::from(13u8),
        };
        let report = build(on_chain, &[account], &[], &markets(), &[]).unwrap();
        assert_eq!(report.equity, U256::from(1_000_500_000u64));
        assert_eq!(report.available, vec![U256::from(250_250_000u64)]);
        assert_eq!(report.positions[0].size, -15_000);
        assert_eq!(report.asOf, 5, "stamped with the chain block's time");
        assert_eq!(report.pending, U256::from(7u8));
        assert_eq!(
            (report.arrived, report.sent),
            (U256::from(11u8), U256::from(13u8))
        );
        // It encodes as the contract's struct does.
        assert!(!report.abi_encode().is_empty());
    }

    #[test]
    fn a_position_in_a_market_the_reader_doesnt_know_is_refused_not_dropped() {
        let account = serde_json::json!({
            "index": 1, "total_asset_value": "1", "available_balance": "1",
            "positions": [{"market_id": 99, "position": "1", "sign": 1}], "shares": []
        });
        assert!(build(OnChain::default(), &[account], &[], &markets(), &[]).is_err());
    }

    #[test]
    fn prices_come_from_the_details_and_the_best_of_each_side() {
        let details = serde_json::json!({"order_book_details": [{"market_id": 26, "mark_price": "775.96", "index_price": "775.00"}]});
        let book =
            serde_json::json!({"bids": [{"price": "775.95"}], "asks": [{"price": "775.97"}]});
        let p = prices(26, &details, &book, &markets()).unwrap();
        assert_eq!(
            (p.mark, p.index, p.bid, p.ask),
            (77_596, 77_500, 77_595, 77_597)
        );
    }

    #[test]
    fn shares_in_a_public_pool_are_posted_at_the_pool_s_worth_per_share() {
        let account = serde_json::json!({
            "index": 10, "total_asset_value": "5", "available_balance": "5", "positions": [],
            "shares": [
                {"public_pool_index": 281474976710654u64, "shares_amount": 90, "entry_usdc": "1"},
                {"public_pool_index": 7, "shares_amount": 0}
            ]
        });
        let pool = serde_json::json!({
            "index": 281474976710654u64, "total_asset_value": "1000.000001",
            "pool_info": {"total_shares": 300}
        });
        assert_eq!(
            pools_held(std::slice::from_ref(&account)),
            vec![281474976710654]
        );
        let report = build(
            OnChain::default(),
            std::slice::from_ref(&account),
            &[],
            &markets(),
            std::slice::from_ref(&pool),
        )
        .unwrap();
        assert_eq!(report.pools.len(), 1, "no shares, nothing posted");
        // 90 of 300 shares of 1000.000001: 300.0000003, rounded down.
        assert_eq!(report.pools[0].value, U256::from(300_000_000u64));
        assert_eq!(report.pools[0].shares, 90);
        // A pool whose account wasn't read, or whose figures can't be, stops
        // the post: its shares are never posted as worth nothing.
        assert!(
            build(
                OnChain::default(),
                std::slice::from_ref(&account),
                &[],
                &markets(),
                &[]
            )
            .is_err()
        );
        for broken in [
            serde_json::json!({"index": 281474976710654u64, "total_asset_value": 1000, "pool_info": {"total_shares": 300}}),
            serde_json::json!({"index": 281474976710654u64, "total_asset_value": "1000", "pool_info": {}}),
            serde_json::json!({"index": 281474976710654u64, "total_asset_value": "1000", "pool_info": {"total_shares": 0}}),
            serde_json::json!({"index": 281474976710654u64, "total_asset_value": "1000", "pool_info": {"total_shares": 10}}),
        ] {
            assert!(
                build(
                    OnChain::default(),
                    std::slice::from_ref(&account),
                    &[],
                    &markets(),
                    std::slice::from_ref(&broken)
                )
                .is_err(),
                "{broken}"
            );
        }
    }

    #[test]
    fn a_price_the_reader_cannot_read_stops_the_post() {
        let book =
            serde_json::json!({"bids": [{"price": "775.95"}], "asks": [{"price": "775.97"}]});
        let detail = |mark: serde_json::Value, index: serde_json::Value| serde_json::json!({"order_book_details": [{"market_id": 26, "mark_price": mark, "index_price": index}]});
        for details in [
            detail(serde_json::json!(775.96), serde_json::json!("775")),
            detail(serde_json::Value::Null, serde_json::json!("775")),
            detail(serde_json::json!("775.96"), serde_json::json!("x")),
            detail(serde_json::json!("0"), serde_json::json!("775")),
            serde_json::json!({"order_book_details": [{"market_id": 27, "mark_price": "1", "index_price": "1"}]}),
            serde_json::json!({}),
        ] {
            assert!(
                prices(26, &details, &book, &markets()).is_err(),
                "{details}"
            );
        }
        let details = detail(serde_json::json!("775.96"), serde_json::json!("775"));
        for book in [
            serde_json::json!({"bids": [{"price": 775.95}], "asks": []}),
            serde_json::json!({"bids": [{}], "asks": []}),
            serde_json::json!({"asks": []}),
        ] {
            assert!(prices(26, &details, &book, &markets()).is_err(), "{book}");
        }
        // An empty side has no best price: zero, which the driver reads as
        // outside the band.
        let empty = serde_json::json!({"bids": [], "asks": [{"price": "775.97"}]});
        assert_eq!(prices(26, &details, &empty, &markets()).unwrap().bid, 0);
    }
}
