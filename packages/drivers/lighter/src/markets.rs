//! Lighter's perp markets: their numbers and how sizes and prices are
//! written in them.
//!
//! The exchange numbers its markets and writes every size and price as an
//! integer with a per-market number of decimals; both differ between its
//! networks, so they are read from the venue, never written down.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use crate::payload::Decimal;

/// One perp market.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Market {
    pub symbol: String,
    pub index: u16,
    pub size_decimals: u8,
    pub price_decimals: u8,
    /// The smallest order, in base units (integer).
    pub min_base: u64,
}

/// Which way to round a price that doesn't fit the market's ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Round {
    /// A buy's limit: never more than asked.
    Down,
    /// A sell's limit: never less than asked.
    Up,
}

impl Market {
    /// `size` in the market's base units, rounded down; refused if it rounds
    /// to less than the smallest order.
    pub fn base_units(&self, size: Decimal) -> Result<u64> {
        if size.is_sign_negative() {
            bail!("a negative size");
        }
        let units = integer((size * pow10(self.size_decimals)).floor())?;
        if units < self.min_base {
            bail!(
                "{size} {} is below the market's smallest order",
                self.symbol
            );
        }
        Ok(units)
    }

    /// `price` in the market's ticks, rounded the safe way for the side.
    pub fn price_ticks(&self, price: Decimal, round: Round) -> Result<u32> {
        if price.is_sign_negative() || price.is_zero() {
            bail!("a price must be positive");
        }
        let scaled = price * pow10(self.price_decimals);
        let ticks = integer(match round {
            Round::Down => scaled.floor(),
            Round::Up => scaled.ceil(),
        })?;
        if ticks == 0 || ticks > u64::from(u32::MAX) {
            bail!("{price} is outside the market's prices");
        }
        Ok(ticks as u32)
    }

    /// A size in base units back to a decimal.
    pub fn size_of(&self, units: i64) -> Decimal {
        Decimal::from(units) / pow10(self.size_decimals)
    }

    /// A price in ticks back to a decimal.
    pub fn price_of(&self, ticks: u64) -> Decimal {
        Decimal::from(ticks) / pow10(self.price_decimals)
    }
}

/// Every perp market, by symbol.
#[derive(Clone, Debug, Default)]
pub struct Markets {
    by_symbol: BTreeMap<String, Market>,
}

impl Markets {
    /// From the exchange's `orderBooks` answer: active perps only.
    pub fn from_order_books(answer: &Value) -> Result<Self> {
        let books = answer["order_books"]
            .as_array()
            .ok_or_else(|| anyhow!("no order_books in the answer"))?;
        let mut by_symbol = BTreeMap::new();
        for book in books {
            if book["market_type"].as_str() != Some("perp")
                || book["status"].as_str() != Some("active")
            {
                continue;
            }
            let symbol = book["symbol"]
                .as_str()
                .ok_or_else(|| anyhow!("a book without a symbol"))?
                .to_string();
            let size_decimals = small(&book["supported_size_decimals"])?;
            let min: Decimal = book["min_base_amount"]
                .as_str()
                .unwrap_or("0")
                .parse()
                .map_err(|_| anyhow!("bad min_base_amount for {symbol}"))?;
            let market = Market {
                symbol: symbol.clone(),
                index: u16::try_from(book["market_id"].as_u64().unwrap_or(u64::MAX))
                    .map_err(|_| anyhow!("bad market id for {symbol}"))?,
                size_decimals,
                price_decimals: small(&book["supported_price_decimals"])?,
                min_base: integer((min * pow10(size_decimals)).ceil())?,
            };
            by_symbol.insert(symbol, market);
        }
        Ok(Self { by_symbol })
    }

    pub fn get(&self, symbol: &str) -> Result<&Market> {
        self.by_symbol
            .get(symbol)
            .ok_or_else(|| anyhow!("{symbol} is not an active perp on this exchange"))
    }

    pub fn by_index(&self, index: u16) -> Option<&Market> {
        self.by_symbol.values().find(|market| market.index == index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Market> {
        self.by_symbol.values()
    }
}

fn small(value: &Value) -> Result<u8> {
    value
        .as_u64()
        .and_then(|n| u8::try_from(n).ok())
        .ok_or_else(|| anyhow!("bad decimals {value}"))
}

pub(crate) fn pow10(digits: u8) -> Decimal {
    Decimal::from(10u64).powi(i32::from(digits))
}

/// A whole, non-negative decimal as a u64.
pub(crate) fn integer(value: Decimal) -> Result<u64> {
    let text = value.to_string();
    let whole = text.split('.').next().unwrap_or("0");
    whole
        .parse::<u64>()
        .map_err(|_| anyhow!("{value} is not a whole number in range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spy() -> Market {
        Market {
            symbol: "SPY".into(),
            index: 26,
            size_decimals: 4,
            price_decimals: 2,
            min_base: 100,
        }
    }

    fn d(text: &str) -> Decimal {
        text.parse().unwrap()
    }

    #[test]
    fn sizes_round_down_and_refuse_less_than_the_smallest_order() {
        assert_eq!(spy().base_units(d("1.23456")).unwrap(), 12_345);
        assert!(spy().base_units(d("0.0099")).is_err());
        assert!(spy().base_units(d("-1")).is_err());
    }

    #[test]
    fn a_buys_limit_rounds_down_and_a_sells_up() {
        assert_eq!(
            spy().price_ticks(d("775.555"), Round::Down).unwrap(),
            77_555
        );
        assert_eq!(spy().price_ticks(d("775.555"), Round::Up).unwrap(), 77_556);
        assert!(spy().price_ticks(d("0"), Round::Up).is_err());
    }

    #[test]
    fn only_active_perps_are_read_from_the_order_books() {
        let answer = serde_json::json!({"order_books": [
            {"symbol": "SPY", "market_id": 26, "market_type": "perp", "status": "active",
             "supported_size_decimals": 4, "supported_price_decimals": 2, "min_base_amount": "0.0100"},
            {"symbol": "OLD", "market_id": 3, "market_type": "perp", "status": "inactive",
             "supported_size_decimals": 4, "supported_price_decimals": 2, "min_base_amount": "1"},
            {"symbol": "ETH/USDG", "market_id": 2048, "market_type": "spot", "status": "active",
             "supported_size_decimals": 4, "supported_price_decimals": 2, "min_base_amount": "1"}
        ]});
        let markets = Markets::from_order_books(&answer).unwrap();
        assert_eq!(markets.get("SPY").unwrap(), &spy());
        assert!(markets.get("OLD").is_err());
        assert!(markets.get("ETH/USDG").is_err());
    }
}
