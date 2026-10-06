//! Robinhood Chain's shared streams: Lighter's order books and Chainlink's
//! per-token price feeds, each one upstream connection fanned out to every
//! agent that subscribes (see [`crate::data`]).
//!
//! - **Lighter**: one WebSocket to the exchange's stream. A book arrives
//!   whole, then as changes (a price level's new size, zero for gone); the
//!   executor keeps each subscribed book and hands every agent the best
//!   levels after each change, stamped with the exchange's own time.
//! - **Chainlink**: the feeds are contracts, so each subscribed feed is read
//!   from a Robinhood Chain node every few seconds and a value is handed on
//!   each new round. A feed often goes hours without one (it moves on a 0.5%
//!   change or a daily heartbeat and holds the last close while the cash
//!   market is shut), so the round's own time travels with its price.
//!
//! Where each is: `SHONIN_LIGHTER_STREAM` (default: Lighter's instance on
//! Robinhood Chain mainnet) and `SHONIN_CHAINLINK_RPC` (default: Robinhood
//! Chain mainnet's public node; the testnet has no feeds). Both are public
//! market data, read only.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::Address;
use alloy::providers::{Provider, ProviderBuilder};
use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use strategy_sdk::streams::{
    Book, CHAINLINK, ChainlinkFeed, ChainlinkValue, LIGHTER, Level, LighterFeed, LighterValue,
};
use strategy_sdk::{Decimal, from_payload, to_payload};

use crate::data::{Outlets, Venue};

/// Lighter's stream on Robinhood Chain mainnet.
pub const LIGHTER_MAINNET: &str = "wss://api.rh.lighter.xyz/stream";
/// Robinhood Chain mainnet's public node.
pub const ROBINHOOD_MAINNET_RPC: &str = "https://rpc.mainnet.chain.robinhood.com";

/// How many levels each side of a book an agent is handed.
const LEVELS: usize = 20;
/// How often each subscribed feed is read.
const READ_EVERY: Duration = Duration::from_secs(10);
/// How long to wait before connecting again, at most.
const RECONNECT_MOST: Duration = Duration::from_secs(30);

/// The Robinhood Chain venues, configured from the environment.
pub fn venues() -> Vec<Arc<dyn Venue>> {
    let lighter = std::env::var("SHONIN_LIGHTER_STREAM").unwrap_or_else(|_| LIGHTER_MAINNET.into());
    let rpc =
        std::env::var("SHONIN_CHAINLINK_RPC").unwrap_or_else(|_| ROBINHOOD_MAINNET_RPC.into());
    vec![
        Arc::new(Lighter { url: lighter }),
        Arc::new(Chainlink { rpc }),
    ]
}

/// Run `connected` for as long as the executor runs, backing off after each
/// failure.
async fn hold<F, Fut>(name: &'static str, mut connected: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mut wait = Duration::from_secs(1);
    loop {
        let began = std::time::Instant::now();
        if let Err(error) = connected().await {
            tracing::warn!(
                venue = name,
                error,
                "the shared stream dropped; connecting again"
            );
        }
        if began.elapsed() > RECONNECT_MOST {
            wait = Duration::from_secs(1);
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(RECONNECT_MOST);
    }
}

// Lighter ---------------------------------------------------------------------

pub struct Lighter {
    pub url: String,
}

/// One book as the exchange last described it: price to size, both sides.
#[derive(Default, Debug, Clone, PartialEq)]
pub struct LocalBook {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
}

impl LocalBook {
    /// Apply a message's levels: a snapshot replaces, an update changes
    /// each level named (a size of zero removes it).
    pub fn apply(&mut self, message: &serde_json::Value, snapshot: bool) {
        if snapshot {
            self.bids.clear();
            self.asks.clear();
        }
        let book = &message["order_book"];
        for (side, levels) in [
            (&mut self.bids, &book["bids"]),
            (&mut self.asks, &book["asks"]),
        ] {
            for level in levels.as_array().into_iter().flatten() {
                let (Some(price), Some(size)) = (decimal(&level["price"]), decimal(&level["size"]))
                else {
                    continue;
                };
                if size.is_zero() {
                    side.remove(&price);
                } else {
                    side.insert(price, size);
                }
            }
        }
    }

    /// The best `LEVELS` each side: bids highest first, asks lowest first.
    pub fn top(&self, market: &str) -> Book {
        let level = |(price, size): (&Decimal, &Decimal)| Level {
            price: *price,
            size: *size,
            orders: None,
        };
        Book {
            market: market.to_string(),
            bids: self.bids.iter().rev().take(LEVELS).map(level).collect(),
            asks: self.asks.iter().take(LEVELS).map(level).collect(),
        }
    }
}

fn decimal(value: &serde_json::Value) -> Option<Decimal> {
    value.as_str()?.parse().ok()
}

impl Venue for Lighter {
    fn name(&self) -> &'static str {
        LIGHTER
    }

    fn feed(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        let feed: LighterFeed =
            from_payload(request).map_err(|error| format!("not a Lighter feed: {error}"))?;
        match feed {
            LighterFeed::Book { market } if market > 4094 => {
                Err(format!("{market} is not a Lighter market"))
            }
            feed => Ok(to_payload(&feed)),
        }
    }

    fn run(self: Arc<Self>, outlets: Arc<Outlets>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let this = self.clone();
            hold(LIGHTER, || {
                let this = this.clone();
                let outlets = outlets.clone();
                async move { this.connected(&outlets).await }
            })
            .await
        })
    }
}

impl Lighter {
    async fn connected(&self, outlets: &Outlets) -> Result<(), String> {
        use tokio_tungstenite::tungstenite::Message as Frame;
        let (socket, _) = tokio_tungstenite::connect_async_tls_with_config(
            self.url.as_str(),
            None,
            false,
            Some(tokio_tungstenite::Connector::Rustls(crate::net::tls())),
        )
        .await
        .map_err(|error| format!("connecting to {}: {error}", self.url))?;
        let (mut sink, mut stream) = socket.split();
        let mut subscribed: BTreeSet<Vec<u8>> = BTreeSet::new();
        let mut books: HashMap<u32, LocalBook> = HashMap::new();
        let mut look = tokio::time::interval(Duration::from_secs(5));
        loop {
            let wanted = outlets.wanted();
            for feed in wanted.difference(&subscribed) {
                let LighterFeed::Book { market } = from_payload(feed)?;
                let message = serde_json::json!({"type": "subscribe", "channel": format!("order_book/{market}")});
                sink.send(Frame::text(message.to_string()))
                    .await
                    .map_err(|error| error.to_string())?;
            }
            for feed in subscribed.difference(&wanted) {
                let LighterFeed::Book { market } = from_payload(feed)?;
                books.remove(&market);
                let message = serde_json::json!({"type": "unsubscribe", "channel": format!("order_book/{market}")});
                sink.send(Frame::text(message.to_string()))
                    .await
                    .map_err(|error| error.to_string())?;
            }
            subscribed = wanted;

            tokio::select! {
                _ = outlets.asked.notified() => {}
                _ = look.tick() => {}
                frame = stream.next() => match frame {
                    Some(Ok(Frame::Text(text))) => {
                        if let Some((market, source_ms, book)) = read_lighter(text.as_str(), &mut books) {
                            let feed = to_payload(&LighterFeed::Book { market });
                            outlets.publish(&feed, source_ms, to_payload(&LighterValue::Book(book)));
                        }
                    }
                    Some(Ok(Frame::Ping(data))) => {
                        sink.send(Frame::Pong(data)).await.map_err(|error| error.to_string())?;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error.to_string()),
                    None => return Err("the venue closed the connection".to_string()),
                },
            }
        }
    }
}

/// What one message from Lighter's stream changes: the market, the
/// exchange's time, and the book after the change. Anything else carries
/// nothing.
pub fn read_lighter(
    text: &str,
    books: &mut HashMap<u32, LocalBook>,
) -> Option<(u32, Option<u64>, Book)> {
    let message: serde_json::Value = serde_json::from_str(text).ok()?;
    let kind = message["type"].as_str()?;
    let snapshot = match kind {
        "subscribed/order_book" => true,
        "update/order_book" => false,
        _ => return None,
    };
    let market: u32 = message["channel"]
        .as_str()?
        .strip_prefix("order_book:")?
        .parse()
        .ok()?;
    let book = books.entry(market).or_default();
    book.apply(&message, snapshot);
    Some((
        market,
        message["timestamp"].as_u64(),
        book.top(&market.to_string()),
    ))
}

// Chainlink -------------------------------------------------------------------

pub struct Chainlink {
    pub rpc: String,
}

alloy::sol! {
    #[sol(rpc)]
    interface IFeed {
        function latestRoundData()
            external
            view
            returns (uint80 roundId, int256 answer, uint256 startedAt, uint256 updatedAt, uint80 answeredInRound);
        function decimals() external view returns (uint8);
    }
}

impl Venue for Chainlink {
    fn name(&self) -> &'static str {
        CHAINLINK
    }

    fn feed(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        let feed: ChainlinkFeed =
            from_payload(request).map_err(|error| format!("not a Chainlink feed: {error}"))?;
        let ChainlinkFeed::Price { token } = &feed;
        if robinhood::listings::feed(robinhood::listings::MAINNET, token).is_none() {
            return Err(format!(
                "there is no Chainlink feed for {token} on Robinhood Chain"
            ));
        }
        // One subscription per feed, whatever the case of the symbol.
        Ok(to_payload(&ChainlinkFeed::Price {
            token: token.to_ascii_uppercase(),
        }))
    }

    fn run(self: Arc<Self>, outlets: Arc<Outlets>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let this = self.clone();
            hold(CHAINLINK, || {
                let this = this.clone();
                let outlets = outlets.clone();
                async move { this.reading(&outlets).await }
            })
            .await
        })
    }
}

impl Chainlink {
    async fn reading(&self, outlets: &Outlets) -> Result<(), String> {
        let url = self
            .rpc
            .parse()
            .map_err(|error| format!("bad RPC URL: {error}"))?;
        let provider = ProviderBuilder::new().connect_http(url).erased();
        let mut last: HashMap<Vec<u8>, u64> = HashMap::new();
        let mut decimals: HashMap<Address, u8> = HashMap::new();
        let mut tick = tokio::time::interval(READ_EVERY);
        loop {
            tokio::select! {
                _ = outlets.asked.notified() => {}
                _ = tick.tick() => {}
            }
            let wanted = outlets.wanted();
            last.retain(|feed, _| wanted.contains(feed));
            for feed in wanted {
                let ChainlinkFeed::Price { token } = from_payload(&feed)?;
                let Some(address) = robinhood::listings::feed(robinhood::listings::MAINNET, &token)
                else {
                    continue;
                };
                let contract = IFeed::new(address, &provider);
                let places = match decimals.get(&address) {
                    Some(places) => *places,
                    None => {
                        let places = contract
                            .decimals()
                            .call()
                            .await
                            .map_err(|error| error.to_string())?;
                        decimals.insert(address, places);
                        places
                    }
                };
                let round = contract
                    .latestRoundData()
                    .call()
                    .await
                    .map_err(|error| error.to_string())?;
                let round_id = u64::try_from(round.roundId).unwrap_or(u64::MAX);
                if last.get(&feed) == Some(&round_id) {
                    continue;
                }
                let Some(value) =
                    chainlink_value(&token, round.answer, places, round.updatedAt, round_id)
                else {
                    continue;
                };
                last.insert(feed.clone(), round_id);
                let updated_ms = u64::try_from(round.updatedAt)
                    .unwrap_or(0)
                    .saturating_mul(1000);
                outlets.publish(&feed, Some(updated_ms), to_payload(&value));
            }
        }
    }
}

/// A round as the stream carries it; nothing for a price that isn't
/// positive.
pub fn chainlink_value(
    token: &str,
    answer: alloy::primitives::I256,
    places: u8,
    updated_at: alloy::primitives::U256,
    round: u64,
) -> Option<ChainlinkValue> {
    if answer.is_negative() || answer.is_zero() {
        return None;
    }
    let raw: Decimal = answer.to_string().parse().ok()?;
    let price = raw / Decimal::from(10u64).powi(i32::from(places));
    Some(ChainlinkValue::Price {
        token: token.to_string(),
        price,
        updated_at: u64::try_from(updated_at).unwrap_or(0),
        round,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::{I256, U256};

    fn d(text: &str) -> Decimal {
        text.parse().unwrap()
    }

    #[test]
    fn a_lighter_book_is_replaced_by_a_snapshot_and_changed_level_by_level_after() {
        let mut books = HashMap::new();
        let snapshot = r#"{"channel":"order_book:26","type":"subscribed/order_book","timestamp":5,
            "order_book":{"asks":[{"price":"775.85","size":"1"},{"price":"775.90","size":"2"}],
                          "bids":[{"price":"775.80","size":"3"},{"price":"775.70","size":"4"}]}}"#;
        let (market, source_ms, book) = read_lighter(snapshot, &mut books).unwrap();
        assert_eq!((market, source_ms), (26, Some(5)));
        assert_eq!(book.bids[0].price, d("775.80"), "bids best first");
        assert_eq!(book.asks[0].price, d("775.85"), "asks best first");

        let update = r#"{"channel":"order_book:26","type":"update/order_book","timestamp":6,
            "order_book":{"asks":[{"price":"775.85","size":"0"}],"bids":[{"price":"775.82","size":"1.5"}]}}"#;
        let (_, _, book) = read_lighter(update, &mut books).unwrap();
        assert_eq!(
            book.asks[0].price,
            d("775.90"),
            "a level of size zero is gone"
        );
        assert_eq!(book.bids[0].price, d("775.82"));
        assert_eq!(book.bids.len(), 3);
    }

    #[test]
    fn messages_that_are_not_books_carry_nothing() {
        let mut books = HashMap::new();
        assert!(read_lighter(r#"{"type":"connected","session_id":"x"}"#, &mut books).is_none());
        assert!(read_lighter("not json", &mut books).is_none());
    }

    #[test]
    fn a_chainlink_round_carries_its_price_in_dollars_and_its_own_time() {
        let value = chainlink_value(
            "NVDA",
            I256::try_from(23_903_014_289i64).unwrap(),
            8,
            U256::from(1_791_230_163u64),
            9,
        )
        .unwrap();
        assert_eq!(
            value,
            ChainlinkValue::Price {
                token: "NVDA".into(),
                price: d("239.03014289"),
                updated_at: 1_791_230_163,
                round: 9,
            }
        );
        assert!(chainlink_value("NVDA", I256::ZERO, 8, U256::ZERO, 1).is_none());
    }

    #[test]
    fn a_chainlink_feed_is_known_by_symbol_in_any_case_and_an_unknown_one_is_refused() {
        let venue = Chainlink { rpc: String::new() };
        let lower = venue
            .feed(&to_payload(&ChainlinkFeed::Price {
                token: "nvda".into(),
            }))
            .unwrap();
        let upper = venue
            .feed(&to_payload(&ChainlinkFeed::Price {
                token: "NVDA".into(),
            }))
            .unwrap();
        assert_eq!(lower, upper, "one upstream read for both");
        assert!(
            venue
                .feed(&to_payload(&ChainlinkFeed::Price {
                    token: "DOGE".into()
                }))
                .is_err()
        );
    }

    /// Against the live venues (public market data): run by hand with
    /// `--ignored`.
    #[tokio::test]
    #[ignore]
    async fn the_live_streams_hand_out_a_lighter_book_and_a_chainlink_price() {
        let hub = crate::data::Hub::new(venues());
        let mut books = hub
            .subscribe(LIGHTER, &to_payload(&LighterFeed::Book { market: 26 }))
            .unwrap();
        let mut prices = hub
            .subscribe(
                CHAINLINK,
                &to_payload(&ChainlinkFeed::Price {
                    token: "NVDA".into(),
                }),
            )
            .unwrap();
        let wait = Duration::from_secs(30);
        let book = tokio::time::timeout(wait, books.next())
            .await
            .unwrap()
            .unwrap();
        let LighterValue::Book(book) = from_payload::<LighterValue>(&book.value).unwrap();
        assert!(!book.bids.is_empty() && !book.asks.is_empty());
        assert!(book.bids[0].price < book.asks[0].price);
        let price = tokio::time::timeout(wait, prices.next())
            .await
            .unwrap()
            .unwrap();
        let ChainlinkValue::Price {
            price, updated_at, ..
        } = from_payload::<ChainlinkValue>(&price.value).unwrap();
        assert!(price > Decimal::from(1u8));
        assert!(updated_at > 1_700_000_000);
    }
}
