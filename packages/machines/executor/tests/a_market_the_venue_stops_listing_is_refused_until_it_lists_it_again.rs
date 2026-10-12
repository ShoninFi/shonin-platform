//! The markets a vault's driver pins are fixed; the venue's own listing is
//! not. For as long as the Hyperliquid half runs, the venue's markets are
//! read again every ten minutes, both ways: a pinned market the venue stops
//! listing for trading is refused from the next look on while the others
//! carry on, and once the venue lists it again it is traded again. Nothing
//! ends the looking, neither a listing with nothing missing nor one that
//! changes back.

#[allow(dead_code, unused_imports)]
#[path = "../src/hyperliquid.rs"]
mod half;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use axum::Json;
use axum::extract::State;
use axum::routing::post;
use hyperliquid::agent::{Allowlist, Trader};
use hyperliquid::payload::{Action, Order, Side, Tif};
use hyperliquid::{Api, Markets, Network};
use serde_json::{Value, json};

/// What the stand-in venue lists, and how often its markets were read.
#[derive(Default)]
struct Venue {
    /// Whether the venue has delisted the second perp, BTC.
    btc_delisted: AtomicBool,
    /// How many times the perps' listing was read.
    looks: AtomicUsize,
}

impl Venue {
    fn meta(&self) -> Value {
        json!({"universe": [
            {"name": "ETH", "szDecimals": 4, "maxLeverage": 25},
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 40,
             "isDelisted": self.btc_delisted.load(Ordering::SeqCst)}
        ]})
    }
}

fn spot_meta() -> Value {
    json!({
        "tokens": [
            {"name": "USDC", "index": 0, "szDecimals": 8, "weiDecimals": 8, "tokenId": "0xaa"},
            {"name": "HYPE", "index": 1105, "szDecimals": 2, "weiDecimals": 8, "tokenId": "0xbb"}
        ],
        "universe": [
            {"name": "@1035", "tokens": [1105, 0], "index": 1035}
        ]
    })
}

async fn info(State(venue): State<Arc<Venue>>, Json(request): Json<Value>) -> Json<Value> {
    Json(match request["type"].as_str() {
        Some("meta") => {
            venue.looks.fetch_add(1, Ordering::SeqCst);
            venue.meta()
        }
        Some("spotMeta") => spot_meta(),
        _ => Value::Null,
    })
}

/// A stand-in for HyperCore's `/info`, serving the venue's markets.
async fn stand_in_venue(venue: Arc<Venue>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new()
        .route("/info", post(info))
        .with_state(venue);
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{address}")
}

fn order(market: &str, price: &str) -> Action {
    Action::Place(Order {
        market: market.to_string(),
        side: Side::Buy,
        size: "0.5".parse().unwrap(),
        limit_price: price.parse().unwrap(),
        tif: Tif::Gtc,
        reduce_only: false,
        client_id: None,
    })
}

/// Wait until `holds` says the trader's allowlist is as expected, for at
/// most a few seconds.
async fn until(trader: &Trader, holds: impl Fn(&Allowlist) -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if holds(&trader.allowlist()) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Wait until the venue's markets have been read `more` times past now.
async fn looks_past(venue: &Venue, more: usize) {
    let target = venue.looks.load(Ordering::SeqCst) + more;
    let started = Instant::now();
    while venue.looks.load(Ordering::SeqCst) < target {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the venue's markets stopped being looked at"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The venue's markets are looked at again every ten minutes.
#[test]
fn the_venue_s_markets_are_looked_at_again_every_ten_minutes() {
    assert_eq!(half::MISSING_LOOK_EVERY, Duration::from_secs(600));
}

/// The driver pins perps ETH and BTC and the HYPE pair, all listed when the
/// half starts. The venue delists BTC: from the next look BTC is refused and
/// ETH and HYPE are still traded, look after look. It lists BTC again: BTC
/// is traded again. It delists BTC once more: refused once more.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_market_the_venue_stops_listing_is_refused_while_the_others_carry_on_and_traded_again_once_it_is_listed_again()
 {
    let venue = Arc::new(Venue::default());
    let url = stand_in_venue(venue.clone()).await;
    let api = Api::at(&url, Network::Testnet);
    let station = Address::repeat_byte(0x55);
    let (perps, spots): (&[u32], &[u32]) = (&[0, 1], &[1035]);
    let markets = Markets::read(&api).await.unwrap();
    let allowlist = Allowlist::from_driver(station, markets, perps, spots).unwrap();
    assert!(allowlist.missing.is_empty());
    let trader = Arc::new(Trader::new(
        api.clone(),
        PrivateKeySigner::random(),
        allowlist,
    ));

    let looking = {
        let (trader, api) = (trader.clone(), api.clone());
        tokio::spawn(async move {
            half::look_for_missing(
                &trader,
                &api,
                station,
                (perps, spots),
                Duration::from_millis(50),
            )
            .await
        })
    };

    // Nothing is missing, and the looking goes on all the same.
    looks_past(&venue, 2).await;
    assert!(trader.allowlist().missing.is_empty());
    assert!(trader.allowlist().check(&order("BTC", "60000"), 1).is_ok());

    // The venue delists BTC.
    venue.btc_delisted.store(true, Ordering::SeqCst);
    assert!(
        until(&trader, |allowlist| allowlist.missing.contains_key("BTC")).await,
        "a market the venue stopped listing was never found missing"
    );
    for _ in 0..2 {
        let allowlist = trader.allowlist();
        let refused = allowlist.check(&order("BTC", "60000"), 1).unwrap_err();
        assert!(refused.to_string().contains("delisted"), "{refused:#}");
        assert!(allowlist.check(&order("ETH", "2700"), 1).is_ok());
        assert!(allowlist.check(&order("HYPE/USDC", "22.5"), 1).is_ok());
        looks_past(&venue, 2).await;
    }

    // The venue lists BTC again.
    venue.btc_delisted.store(false, Ordering::SeqCst);
    assert!(
        until(&trader, |allowlist| allowlist.missing.is_empty()).await,
        "a market the venue lists again was never taken back"
    );
    let allowlist = trader.allowlist();
    assert!(allowlist.check(&order("BTC", "60000"), 1).is_ok());
    assert!(allowlist.check(&order("ETH", "2700"), 1).is_ok());

    // Nothing missing ends nothing: the venue delists BTC once more.
    looks_past(&venue, 2).await;
    venue.btc_delisted.store(true, Ordering::SeqCst);
    assert!(
        until(&trader, |allowlist| allowlist.missing.contains_key("BTC")).await,
        "a market delisted a second time was not found missing again"
    );
    assert!(trader.allowlist().check(&order("BTC", "60000"), 1).is_err());
    assert!(trader.allowlist().check(&order("ETH", "2700"), 1).is_ok());
    assert!(!looking.is_finished(), "the looking ended");
    looking.abort();
}
