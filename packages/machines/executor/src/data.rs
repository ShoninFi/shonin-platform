//! Shonin's shared venue streams: one connection per venue, fanned out to
//! every agent that subscribes.
//!
//! Venues limit what one address may ask of them (Hyperliquid: 10
//! WebSockets and 1,000 subscriptions per IP), and every agent on a machine
//! shares its address. So the executor holds one connection to each venue,
//! subscribes upstream once to each feed any agent wants, and hands every
//! value to each agent subscribed to it, stamped with when it arrived.
//!
//! A venue is a [`Venue`]: it reads an agent's feed request into the
//! canonical bytes one upstream subscription is shared under, and runs its
//! connection, subscribing to whatever [`Outlets::wanted`] holds and
//! publishing each value through [`Outlets::publish`]. Hyperliquid's is
//! here; the others are added the same way.
//!
//! Nothing is promised beyond each value's stamps: an agent that reads too
//! slowly is handed the newest values, and the ones it missed are dropped.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{SinkExt, Stream, StreamExt};
use strategy_sdk::streams::{
    Book, Funding, HYPERLIQUID, HyperliquidFeed, HyperliquidValue, Level, Mid,
};
use strategy_sdk::{Decimal, from_payload, to_payload};
use tokio::sync::{Notify, broadcast};

/// How many values wait for a slow agent before the oldest are dropped.
const WAITING_MOST: usize = 256;

/// One value from a venue, stamped.
#[derive(Clone, Debug, PartialEq)]
pub struct Stamped {
    /// When it arrived, in milliseconds since the Unix epoch.
    pub received_ms: u64,
    /// When the venue says it was made.
    pub source_ms: Option<u64>,
    /// In the SDK's payload format for the venue.
    pub value: Vec<u8>,
}

/// The time now, in milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

/// A venue's shared stream.
pub trait Venue: Send + Sync + 'static {
    /// The name agents subscribe by.
    fn name(&self) -> &'static str;

    /// An agent's feed request, as the canonical bytes one upstream
    /// subscription is shared under, or why it isn't a feed this venue has.
    fn feed(&self, request: &[u8]) -> Result<Vec<u8>, String>;

    /// Hold the venue's connection for as long as the executor runs:
    /// subscribe to what `outlets` wants, publish what arrives, and connect
    /// again after a failure.
    fn run(self: Arc<Self>, outlets: Arc<Outlets>) -> BoxFuture<'static, ()>;
}

/// Where a venue publishes, and what it is asked for.
#[derive(Default)]
pub struct Outlets {
    feeds: Mutex<HashMap<Vec<u8>, broadcast::Sender<Stamped>>>,
    /// Told when an agent subscribes to a feed nobody had.
    pub asked: Notify,
}

impl Outlets {
    fn feeds(&self) -> std::sync::MutexGuard<'_, HashMap<Vec<u8>, broadcast::Sender<Stamped>>> {
        self.feeds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The feeds someone is subscribed to now. A feed everyone has let go of
    /// is forgotten here, so the venue can let go of it upstream.
    pub fn wanted(&self) -> BTreeSet<Vec<u8>> {
        let mut feeds = self.feeds();
        feeds.retain(|_, sender| sender.receiver_count() > 0);
        feeds.keys().cloned().collect()
    }

    /// Hand `value` to everyone subscribed to `feed`, stamped with now.
    pub fn publish(&self, feed: &[u8], source_ms: Option<u64>, value: Vec<u8>) {
        let feeds = self.feeds();
        if let Some(sender) = feeds.get(feed) {
            let _ = sender.send(Stamped {
                received_ms: now_ms(),
                source_ms,
                value,
            });
        }
    }

    fn subscribe(&self, feed: Vec<u8>) -> broadcast::Receiver<Stamped> {
        let mut feeds = self.feeds();
        let fresh = feeds
            .get(&feed)
            .is_none_or(|sender| sender.receiver_count() == 0);
        let receiver = feeds
            .entry(feed)
            .or_insert_with(|| broadcast::channel(WAITING_MOST).0)
            .subscribe();
        drop(feeds);
        if fresh {
            self.asked.notify_one();
        }
        receiver
    }
}

/// Every venue this executor carries, each started on first use.
pub struct Hub {
    venues: HashMap<&'static str, (Arc<dyn Venue>, Arc<Outlets>)>,
    started: Mutex<BTreeSet<&'static str>>,
}

impl Hub {
    pub fn new(venues: Vec<Arc<dyn Venue>>) -> Self {
        Self {
            venues: venues
                .into_iter()
                .map(|venue| (venue.name(), (venue, Arc::new(Outlets::default()))))
                .collect(),
            started: Mutex::new(BTreeSet::new()),
        }
    }

    /// Subscribe to `request` on `source`: every value published for it from
    /// now on, the oldest dropped if the reader falls behind.
    pub fn subscribe(
        &self,
        source: &str,
        request: &[u8],
    ) -> Result<impl Stream<Item = Stamped> + Send + Unpin + use<>, String> {
        let (venue, outlets) = self.venues.get(source).ok_or_else(|| {
            let mut known: Vec<&str> = self.venues.keys().copied().collect();
            known.sort();
            format!("there is no shared stream called {source}; there are {known:?}")
        })?;
        let feed = venue.feed(request)?;
        let receiver = outlets.subscribe(feed);
        let mut started = self
            .started
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if started.insert(venue.name()) {
            tokio::spawn(venue.clone().run(outlets.clone()));
        }
        Ok(tokio_stream::wrappers::BroadcastStream::new(receiver)
            .filter_map(|value| std::future::ready(value.ok())))
    }
}

/// Hyperliquid's public WebSocket on its testnet.
pub const HYPERLIQUID_TESTNET: &str = "wss://api.hyperliquid-testnet.xyz/ws";
/// And on mainnet.
pub const HYPERLIQUID_MAINNET: &str = "wss://api.hyperliquid.xyz/ws";

static HUB: OnceLock<Hub> = OnceLock::new();
static HYPERLIQUID_URL: OnceLock<String> = OnceLock::new();

/// Say where Hyperliquid's stream is, before any agent subscribes.
pub fn configure(hyperliquid: String) {
    let _ = HYPERLIQUID_URL.set(hyperliquid);
}

/// Each spot pair's name as an agent names it in an order (`HYPE/USDC`), and
/// the venue's own name for it in its feeds (`@1035` on testnet, `@107` on
/// mainnet: the venue numbers pairs per network). Read from the venue.
static SPOT_COINS: std::sync::RwLock<BTreeMap<String, String>> =
    std::sync::RwLock::new(BTreeMap::new());

/// Learn the spot pairs' feed names from the venue's markets, so an agent
/// can ask for a pair's book by the name its orders use, on any network.
pub fn name_spot_pairs(markets: &::hyperliquid::Markets) {
    let coins: BTreeMap<String, String> = markets
        .spot()
        .map(|market| (market.name.clone(), market.coin.clone()))
        .collect();
    *SPOT_COINS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = coins;
}

/// The venue's feed name for `coin`: a pair named as orders name it
/// (`BASE/USDC`) becomes the venue's own name for it; anything else is
/// already the venue's name.
fn venue_coin(coin: &str) -> Result<String, String> {
    if !coin.contains('/') {
        return Ok(coin.to_string());
    }
    SPOT_COINS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(coin)
        .cloned()
        .ok_or_else(|| format!("{coin:?} is not a spot pair the venue lists (or its pairs aren't read yet)"))
}

/// The executor's hub.
pub fn hub() -> &'static Hub {
    HUB.get_or_init(|| {
        let hyperliquid = HYPERLIQUID_URL
            .get()
            .cloned()
            .unwrap_or_else(|| HYPERLIQUID_TESTNET.to_string());
        let mut venues: Vec<Arc<dyn Venue>> = vec![Arc::new(Hyperliquid { url: hyperliquid })];
        venues.extend(crate::streams_rh::venues());
        Hub::new(venues)
    })
}

// Hyperliquid -----------------------------------------------------------------

/// Hyperliquid's mids, books and funding, over one WebSocket.
pub struct Hyperliquid {
    pub url: String,
}

/// How often Hyperliquid is pinged: it closes a connection quiet for a
/// minute.
const PING_EVERY: Duration = Duration::from_secs(30);

/// How long to wait before connecting again, at most.
const RECONNECT_MOST: Duration = Duration::from_secs(30);

impl Venue for Hyperliquid {
    fn name(&self) -> &'static str {
        HYPERLIQUID
    }

    fn feed(&self, request: &[u8]) -> Result<Vec<u8>, String> {
        let feed: HyperliquidFeed =
            from_payload(request).map_err(|error| format!("not a Hyperliquid feed: {error}"))?;
        let feed = match feed {
            HyperliquidFeed::Book { coin } | HyperliquidFeed::Funding { coin }
                if coin.is_empty() || coin.len() > 32 =>
            {
                return Err(format!("{coin:?} is not a Hyperliquid coin"));
            }
            HyperliquidFeed::Book { coin } => HyperliquidFeed::Book {
                coin: venue_coin(&coin)?,
            },
            HyperliquidFeed::Funding { coin } => HyperliquidFeed::Funding {
                coin: venue_coin(&coin)?,
            },
            other => other,
        };
        Ok(to_payload(&feed))
    }

    fn run(self: Arc<Self>, outlets: Arc<Outlets>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let mut wait = Duration::from_secs(1);
            loop {
                let began = std::time::Instant::now();
                if let Err(error) = self.connected(&outlets).await {
                    tracing::warn!(
                        venue = HYPERLIQUID,
                        error,
                        "the shared stream dropped; connecting again"
                    );
                }
                // A connection that held a while starts the backoff again.
                if began.elapsed() > RECONNECT_MOST {
                    wait = Duration::from_secs(1);
                }
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(RECONNECT_MOST);
            }
        })
    }
}

impl Hyperliquid {
    /// One connection, for as long as it lasts.
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
        let mut ping = tokio::time::interval(PING_EVERY);
        let mut look = tokio::time::interval(Duration::from_secs(5));
        loop {
            // Bring the upstream subscriptions in line with what is wanted.
            let wanted = outlets.wanted();
            for feed in wanted.difference(&subscribed) {
                let message = subscription("subscribe", feed)?;
                sink.send(Frame::text(message))
                    .await
                    .map_err(|error| error.to_string())?;
            }
            for feed in subscribed.difference(&wanted) {
                let message = subscription("unsubscribe", feed)?;
                sink.send(Frame::text(message))
                    .await
                    .map_err(|error| error.to_string())?;
            }
            subscribed = wanted;

            tokio::select! {
                _ = outlets.asked.notified() => {}
                _ = look.tick() => {}
                _ = ping.tick() => {
                    sink.send(Frame::text(r#"{"method":"ping"}"#))
                        .await
                        .map_err(|error| error.to_string())?;
                }
                frame = stream.next() => match frame {
                    Some(Ok(Frame::Text(text))) => {
                        for (feed, source_ms, value) in read_hyperliquid(text.as_str()) {
                            outlets.publish(&feed, source_ms, to_payload(&value));
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(error.to_string()),
                    None => return Err("the venue closed the connection".to_string()),
                },
            }
        }
    }
}

/// The message that subscribes to (or unsubscribes from) a feed.
fn subscription(method: &str, feed: &[u8]) -> Result<String, String> {
    let feed: HyperliquidFeed = from_payload(feed)?;
    let subscription = match feed {
        HyperliquidFeed::Mids => serde_json::json!({ "type": "allMids" }),
        HyperliquidFeed::Book { coin } => serde_json::json!({ "type": "l2Book", "coin": coin }),
        HyperliquidFeed::Funding { coin } => {
            serde_json::json!({ "type": "activeAssetCtx", "coin": coin })
        }
    };
    Ok(serde_json::json!({ "method": method, "subscription": subscription }).to_string())
}

/// What one message from Hyperliquid carries: each value, with the feed it
/// belongs to and the venue's own time where it gives one. Anything else
/// (acknowledgements, pongs, what can't be read) carries nothing.
pub fn read_hyperliquid(text: &str) -> Vec<(Vec<u8>, Option<u64>, HyperliquidValue)> {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let data = &message["data"];
    let decimal = |value: &serde_json::Value| -> Option<Decimal> { value.as_str()?.parse().ok() };
    match message["channel"].as_str() {
        Some("allMids") => {
            let Some(mids) = data["mids"].as_object() else {
                return Vec::new();
            };
            let mut mids: Vec<Mid> = mids
                .iter()
                .filter_map(|(coin, price)| {
                    Some(Mid {
                        market: coin.clone(),
                        price: decimal(price)?,
                    })
                })
                .collect();
            mids.sort_by(|a, b| a.market.cmp(&b.market));
            vec![(
                to_payload(&HyperliquidFeed::Mids),
                None,
                HyperliquidValue::Mids(mids),
            )]
        }
        Some("l2Book") => {
            let Some(coin) = data["coin"].as_str() else {
                return Vec::new();
            };
            let side = |levels: &serde_json::Value| -> Vec<Level> {
                levels
                    .as_array()
                    .map(|levels| {
                        levels
                            .iter()
                            .filter_map(|level| {
                                Some(Level {
                                    price: decimal(&level["px"])?,
                                    size: decimal(&level["sz"])?,
                                    orders: level["n"].as_u64().map(|n| n as u32),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let book = Book {
                market: coin.to_string(),
                bids: side(&data["levels"][0]),
                asks: side(&data["levels"][1]),
            };
            vec![(
                to_payload(&HyperliquidFeed::Book {
                    coin: coin.to_string(),
                }),
                data["time"].as_u64(),
                HyperliquidValue::Book(book),
            )]
        }
        Some("activeAssetCtx") => {
            let (Some(coin), context) = (data["coin"].as_str(), &data["ctx"]) else {
                return Vec::new();
            };
            let (Some(hourly_rate), Some(mark), Some(oracle), Some(open_interest)) = (
                decimal(&context["funding"]),
                decimal(&context["markPx"]),
                decimal(&context["oraclePx"]),
                decimal(&context["openInterest"]),
            ) else {
                return Vec::new();
            };
            vec![(
                to_payload(&HyperliquidFeed::Funding {
                    coin: coin.to_string(),
                }),
                None,
                HyperliquidValue::Funding(Funding {
                    market: coin.to_string(),
                    hourly_rate,
                    mark,
                    oracle,
                    open_interest,
                    premium: decimal(&context["premium"]),
                }),
            )]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strategy_sdk::decimal;

    /// A venue that publishes, for each feed wanted, a counter every few
    /// milliseconds, and counts how often it was started.
    struct Ticking {
        started: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Venue for Ticking {
        fn name(&self) -> &'static str {
            "ticking"
        }

        fn feed(&self, request: &[u8]) -> Result<Vec<u8>, String> {
            if request.is_empty() {
                Err("no feed named".to_string())
            } else {
                Ok(request.to_vec())
            }
        }

        fn run(self: Arc<Self>, outlets: Arc<Outlets>) -> BoxFuture<'static, ()> {
            self.started
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                let mut tick = 0u8;
                loop {
                    for feed in outlets.wanted() {
                        outlets.publish(&feed, Some(u64::from(tick)), vec![tick]);
                    }
                    tick = tick.wrapping_add(1);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
        }
    }

    fn hub() -> (Hub, Arc<std::sync::atomic::AtomicUsize>) {
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hub = Hub::new(vec![Arc::new(Ticking {
            started: started.clone(),
        })]);
        (hub, started)
    }

    #[tokio::test]
    async fn one_venue_connection_is_fanned_out_to_two_subscribers() {
        let (hub, started) = hub();
        let mut one = hub.subscribe("ticking", b"feed").unwrap();
        let mut two = hub.subscribe("ticking", b"feed").unwrap();
        // Both are handed the same values from the one upstream.
        let a = one.next().await.unwrap();
        let mut b = two.next().await.unwrap();
        while b.value != a.value {
            b = two.next().await.unwrap();
        }
        assert_eq!(a.source_ms, b.source_ms);
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 1);
        // One feed upstream, whoever listens.
        assert_eq!(hub.venues["ticking"].1.wanted().len(), 1);
    }

    #[tokio::test]
    async fn every_value_is_stamped_with_when_it_arrived() {
        let (hub, _) = hub();
        let before = now_ms();
        let mut stream = hub.subscribe("ticking", b"feed").unwrap();
        for _ in 0..5 {
            let value = stream.next().await.unwrap();
            assert!(value.received_ms >= before, "{value:?} has no stamp");
            assert!(value.received_ms <= now_ms());
            assert!(value.source_ms.is_some());
        }
    }

    #[tokio::test]
    async fn a_feed_nobody_listens_to_is_let_go() {
        let (hub, _) = hub();
        let stream = hub.subscribe("ticking", b"feed").unwrap();
        let other = hub.subscribe("ticking", b"other").unwrap();
        assert_eq!(hub.venues["ticking"].1.wanted().len(), 2);
        drop(stream);
        assert_eq!(
            hub.venues["ticking"].1.wanted(),
            BTreeSet::from([b"other".to_vec()])
        );
        drop(other);
        assert!(hub.venues["ticking"].1.wanted().is_empty());
    }

    #[test]
    fn an_unknown_source_or_feed_is_refused() {
        let (hub, _) = hub();
        let unknown = hub.subscribe("nowhere", b"feed").err().unwrap();
        assert!(
            unknown.contains("no shared stream called nowhere"),
            "{unknown}"
        );
        assert_eq!(
            hub.subscribe("ticking", b"").err().unwrap(),
            "no feed named"
        );
        let refused = Hyperliquid { url: String::new() }
            .feed(b"not cbor")
            .unwrap_err();
        assert!(refused.contains("not a Hyperliquid feed"), "{refused}");
    }

    #[test]
    fn hyperliquid_s_books_mids_and_funding_are_read_with_their_feed() {
        let book = r#"{"channel":"l2Book","data":{"coin":"BTC","time":1759700000000,
            "levels":[[{"px":"64000.5","sz":"0.25","n":3}],[{"px":"64001","sz":"1.5","n":1}]]}}"#;
        let read = read_hyperliquid(book);
        assert_eq!(read.len(), 1);
        let (feed, at, value) = &read[0];
        assert_eq!(
            feed,
            &to_payload(&HyperliquidFeed::Book {
                coin: "BTC".to_string()
            })
        );
        assert_eq!(*at, Some(1_759_700_000_000));
        let HyperliquidValue::Book(book) = value else {
            panic!("{value:?}")
        };
        assert_eq!(book.bids[0].price, decimal("64000.5"));
        assert_eq!(book.asks[0].size, decimal("1.5"));
        assert_eq!(book.bids[0].orders, Some(3));

        let mids = r#"{"channel":"allMids","data":{"mids":{"ETH":"2500.1","BTC":"64000"}}}"#;
        let (feed, _, value) = &read_hyperliquid(mids)[0];
        assert_eq!(feed, &to_payload(&HyperliquidFeed::Mids));
        assert_eq!(
            value,
            &HyperliquidValue::Mids(vec![
                Mid {
                    market: "BTC".to_string(),
                    price: decimal("64000")
                },
                Mid {
                    market: "ETH".to_string(),
                    price: decimal("2500.1")
                },
            ])
        );

        let funding = r#"{"channel":"activeAssetCtx","data":{"coin":"ETH","ctx":{
            "funding":"0.0000125","openInterest":"1000.5","oraclePx":"2500","markPx":"2501",
            "premium":"0.0004","dayNtlVlm":"1"}}}"#;
        let (_, _, value) = &read_hyperliquid(funding)[0];
        assert_eq!(
            value,
            &HyperliquidValue::Funding(Funding {
                market: "ETH".to_string(),
                hourly_rate: decimal("0.0000125"),
                mark: decimal("2501"),
                oracle: decimal("2500"),
                open_interest: decimal("1000.5"),
                premium: Some(decimal("0.0004")),
            })
        );

        for nothing in [
            r#"{"channel":"subscriptionResponse","data":{}}"#,
            r#"{"channel":"pong"}"#,
            "not json",
        ] {
            assert!(read_hyperliquid(nothing).is_empty());
        }
    }

    /// Against Hyperliquid's public testnet stream, so not run by default:
    /// `cargo test -p shonin-executor --bin executor -- --ignored hyperliquid_testnet`.
    #[tokio::test]
    #[ignore = "reaches Hyperliquid's public testnet"]
    async fn the_hyperliquid_testnet_stream_hands_out_books_mids_and_funding() {
        let hub = Hub::new(vec![Arc::new(Hyperliquid {
            url: HYPERLIQUID_TESTNET.to_string(),
        })]);
        let feeds = [
            HyperliquidFeed::Mids,
            HyperliquidFeed::Book {
                coin: "BTC".to_string(),
            },
            HyperliquidFeed::Funding {
                coin: "BTC".to_string(),
            },
        ];
        for feed in feeds {
            let mut values = hub.subscribe(HYPERLIQUID, &to_payload(&feed)).unwrap();
            let value = tokio::time::timeout(Duration::from_secs(30), values.next())
                .await
                .unwrap_or_else(|_| panic!("nothing for {feed:?} in 30 s"))
                .unwrap();
            let read: HyperliquidValue = from_payload(&value.value).unwrap();
            println!(
                "{feed:?} at {} ({:?}): {read:?}",
                value.received_ms, value.source_ms
            );
            match (&feed, read) {
                (HyperliquidFeed::Mids, HyperliquidValue::Mids(mids)) => assert!(!mids.is_empty()),
                (HyperliquidFeed::Book { .. }, HyperliquidValue::Book(book)) => {
                    assert_eq!(book.market, "BTC");
                    assert!(!book.bids.is_empty() || !book.asks.is_empty());
                }
                (HyperliquidFeed::Funding { .. }, HyperliquidValue::Funding(funding)) => {
                    assert!(funding.mark > Decimal::from(0));
                }
                (feed, read) => panic!("{feed:?} carried {read:?}"),
            }
        }
    }

    #[test]
    fn a_spot_pair_s_book_is_asked_for_by_the_name_orders_use_and_reaches_the_venue_by_its_own() {
        let meta = serde_json::json!({"universe": [
            {"name": "ETH", "szDecimals": 4, "maxLeverage": 25}
        ]});
        let spot = serde_json::json!({
            "tokens": [
                {"name": "USDC", "index": 0, "szDecimals": 8, "weiDecimals": 8, "tokenId": "0xaa"},
                {"name": "HYPE", "index": 1105, "szDecimals": 2, "weiDecimals": 8, "tokenId": "0xbb"}
            ],
            "universe": [{"name": "@1035", "tokens": [1105, 0], "index": 1035}]
        });
        name_spot_pairs(&::hyperliquid::Markets::from_meta(&meta, &spot).unwrap());
        let venue = Hyperliquid { url: String::new() };
        let asked = |coin: &str| {
            venue.feed(&to_payload(&HyperliquidFeed::Book {
                coin: coin.to_string(),
            }))
        };
        assert_eq!(
            asked("HYPE/USDC").unwrap(),
            to_payload(&HyperliquidFeed::Book {
                coin: "@1035".to_string()
            }),
            "the same feed as the venue's own name, so both share one subscription"
        );
        assert_eq!(asked("@1035").unwrap(), asked("HYPE/USDC").unwrap());
        assert_eq!(
            asked("ETH").unwrap(),
            to_payload(&HyperliquidFeed::Book {
                coin: "ETH".to_string()
            })
        );
        assert!(asked("NOPE/USDC").is_err(), "a pair the venue doesn't list");
    }

    #[test]
    fn subscribing_upstream_names_the_feed_as_hyperliquid_does() {
        let book = to_payload(&HyperliquidFeed::Book {
            coin: "BTC".to_string(),
        });
        let message: serde_json::Value =
            serde_json::from_str(&subscription("subscribe", &book).unwrap()).unwrap();
        assert_eq!(
            message,
            serde_json::json!({"method":"subscribe","subscription":{"type":"l2Book","coin":"BTC"}})
        );
    }
}
