//! The attested read: what an executor or unwinder image reads from
//! Lighter's API about a station's accounts and posts on the board, where
//! the driver values the station from it.
//!
//! Everything is posted in the venue's own integer units (sizes in base
//! units, prices in ticks), exactly as the API gives them, so the contract
//! does the arithmetic and nothing is rounded twice.

use alloy::primitives::{Address, U256};
use anyhow::{Result, anyhow};
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

/// Dollars (USDG, six decimals) from the API's decimal text.
fn dollars(value: &Value) -> Result<U256> {
    let text = value.as_str().unwrap_or("0");
    let amount: Decimal = text.parse().map_err(|_| anyhow!("bad amount {text}"))?;
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
/// read on-chain beside them and each held market's prices.
pub fn build(
    on_chain: OnChain,
    accounts: &[Value],
    prices: &[Prices],
    markets: &Markets,
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
            .ok_or_else(|| anyhow!("an account without an index"))?;
        let index = alloy::primitives::Uint::<48, 1>::from(index);
        report.accounts.push(index);
        report.equity += dollars(&account["total_asset_value"])?;
        report
            .available
            .push(dollars(&account["available_balance"])?);
        for position in account["positions"].as_array().into_iter().flatten() {
            let market_id = position["market_id"].as_u64().unwrap_or(u64::MAX);
            let market = u16::try_from(market_id)
                .ok()
                .and_then(|id| markets.by_index(id))
                .ok_or_else(|| anyhow!("a position in unknown market {market_id}"))?;
            let size: Decimal = position["position"]
                .as_str()
                .unwrap_or("0")
                .parse()
                .map_err(|_| anyhow!("bad position size"))?;
            let units = integer((size * pow10(market.size_decimals)).floor())? as i64;
            if units == 0 {
                continue;
            }
            let signed = if position["sign"].as_i64() == Some(-1) {
                -units
            } else {
                units
            };
            report.positions.push(LighterPosition {
                account: index,
                market: market.index,
                size: signed,
            });
        }
        for share in account["shares"].as_array().into_iter().flatten() {
            let pool = share["public_pool_index"].as_u64().unwrap_or(0);
            let shares = share["shares_amount"].as_u64().unwrap_or(0);
            if shares == 0 {
                continue;
            }
            report.pools.push(LighterShares {
                account: index,
                pool: alloy::primitives::Uint::<48, 1>::from(pool),
                shares,
                value: dollars(&share["value"])?,
            });
        }
    }
    Ok(report)
}

/// A market's prices from its details (mark, index) and its book's best
/// orders.
pub fn prices(market: u16, details: &Value, book: &Value, markets: &Markets) -> Result<Prices> {
    let m = markets
        .by_index(market)
        .ok_or_else(|| anyhow!("unknown market {market}"))?;
    let detail = &details["order_book_details"][0];
    let ticks = |value: &Value| -> Result<u32> {
        let text = match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let price: Decimal = text.parse().map_err(|_| anyhow!("bad price {text}"))?;
        let ticks = integer((price * pow10(m.price_decimals)).round(0))?;
        u32::try_from(ticks).map_err(|_| anyhow!("price out of range"))
    };
    let best = |side: &str| -> Result<u32> {
        book[side]
            .get(0)
            .map(|order| ticks(&order["price"]))
            .unwrap_or(Ok(0))
    };
    Ok(Prices {
        market,
        mark: ticks(&detail["mark_price"])?,
        index: ticks(&detail["index_price"])?,
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
        let report = build(on_chain, &[account], &[], &markets()).unwrap();
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
            "positions": [{"market_id": 99, "position": "1", "sign": 1}]
        });
        assert!(build(OnChain::default(), &[account], &[], &markets()).is_err());
    }

    #[test]
    fn prices_come_from_the_details_and_the_best_of_each_side() {
        let details = serde_json::json!({"order_book_details": [{"mark_price": "775.96", "index_price": "775.00"}]});
        let book =
            serde_json::json!({"bids": [{"price": "775.95"}], "asks": [{"price": "775.97"}]});
        let p = prices(26, &details, &book, &markets()).unwrap();
        assert_eq!(
            (p.mark, p.index, p.bid, p.ask),
            (77_596, 77_500, 77_595, 77_597)
        );
    }
}
