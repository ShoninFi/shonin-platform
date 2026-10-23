//! Shonin's shared venue streams, as [`crate::data::subscribe`] hands them
//! over.
//!
//! The executor holds one connection to each venue, whoever listens, and
//! hands every value to each agent that subscribed, stamped with when it
//! arrived (and when the venue says it was made, where it says so). Nothing
//! is promised about freshness beyond those stamps: an agent that falls
//! behind is handed the newest values, not every one.
//!
//! ```ignore
//! use strategy_sdk::streams::{self, HyperliquidFeed, HyperliquidValue};
//! let mut books = streams::subscribe::<_, HyperliquidValue>(
//!     streams::HYPERLIQUID,
//!     &HyperliquidFeed::Book { coin: "BTC".into() },
//! )?;
//! while let Some(book) = books.next().await { /* book.received_ms, book.value */ }
//! ```

use futures::{Stream, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::payload::{Decimal, from_payload, to_payload};

/// Hyperliquid's prices, books and funding.
pub const HYPERLIQUID: &str = "hyperliquid";
/// Lighter's order books.
pub const LIGHTER: &str = "lighter";
/// Chainlink's per-token price feeds on Robinhood Chain.
pub const CHAINLINK: &str = "chainlink";

/// One value from a stream, with when it arrived.
#[derive(Clone, Debug, PartialEq)]
pub struct Stamped<T> {
    /// When the executor received it, in milliseconds since the Unix epoch.
    pub received_ms: u64,
    /// When the venue says it was made, where it says so.
    pub source_ms: Option<u64>,
    pub value: T,
}

/// Subscribe to `feed` on `source`, reading each value as a `V`. A value
/// that doesn't read as one comes through as an error, so the agent decides
/// what to do about it.
pub fn subscribe<F: Serialize, V: DeserializeOwned>(
    source: &str,
    feed: &F,
) -> Result<impl Stream<Item = Result<Stamped<V>, String>> + use<F, V>, String> {
    let reader = crate::data::subscribe(source, &to_payload(feed))?;
    Ok(reader.into_stream().map(|stamped| {
        Ok(Stamped {
            received_ms: stamped.received_ms,
            source_ms: stamped.source_ms,
            value: from_payload(&stamped.value)?,
        })
    }))
}

/// A price level of a book.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Level {
    pub price: Decimal,
    pub size: Decimal,
    /// How many orders make it up, where the venue says.
    pub orders: Option<u32>,
}

/// One market's book: bids best first, then asks best first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub market: String,
    pub bids: Vec<Level>,
    pub asks: Vec<Level>,
}

/// One market's mid price.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mid {
    pub market: String,
    pub price: Decimal,
}

/// A perpetual's funding and prices.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Funding {
    pub market: String,
    /// What longs pay shorts each hour, as a fraction of notional.
    pub hourly_rate: Decimal,
    pub mark: Decimal,
    pub oracle: Decimal,
    /// In units of the asset.
    pub open_interest: Decimal,
    pub premium: Option<Decimal>,
}

/// What can be asked of Hyperliquid's stream.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HyperliquidFeed {
    /// Every market's mid price, whenever any changes.
    Mids,
    /// One market's book: a perp by its coin (`BTC`), or a spot pair by the
    /// name orders use (`HYPE/USDC`), which the executor turns into the
    /// venue's own name for it on the network it serves (or by that name,
    /// `@107`, which differs between networks).
    Book { coin: String },
    /// One perpetual's funding, mark and oracle prices.
    Funding { coin: String },
}

/// What Hyperliquid's stream carries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HyperliquidValue {
    Mids(Vec<Mid>),
    Book(Book),
    Funding(Funding),
}

/// What can be asked of Lighter's stream.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LighterFeed {
    /// One market's book, by Lighter's market index.
    Book { market: u32 },
}

/// What Lighter's stream carries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LighterValue {
    Book(Book),
}

/// What can be asked of the Chainlink stream.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChainlinkFeed {
    /// One token's price feed, by the token's symbol (such as `TSLA`).
    Price { token: String },
}

/// What the Chainlink stream carries: each new round of a feed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChainlinkValue {
    Price {
        token: String,
        price: Decimal,
        /// The round's own time, in seconds since the Unix epoch. A feed
        /// can go hours without a new round while its market is shut.
        updated_at: u64,
        /// The round within its feed's current phase.
        round: u64,
    },
}
