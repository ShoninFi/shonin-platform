//! What the service reports on `/health`: each chain's gas, each mirror's
//! round against its mainnet feed's, and each pool's price against its
//! feed's. It answers 503 while a round has not finished cleanly lately, so
//! the testnet watch sees a market that has stopped keeping its pools.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// One mirror feed as last read.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FeedHealth {
    pub symbol: String,
    /// The mainnet feed's latest round and the mirror's, as decimal ids.
    pub source_round: String,
    pub mirror_round: String,
    pub source_updated_at: u64,
    pub mirror_updated_at: u64,
    /// When this service last posted a round to it (unix seconds).
    pub last_post: Option<u64>,
}

/// One pool as last read.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PoolHealth {
    pub symbol: String,
    pub pool: String,
    /// The feed's price and the pool's, in dollars per whole token.
    pub reference: f64,
    pub pool_price: f64,
    /// How far the pool prices the token above its feed, in basis points.
    pub premium_bps: f64,
    pub feed_updated_at: u64,
    /// When this service last traded it (unix seconds).
    pub last_trade: Option<u64>,
    /// Why the pool can't be moved one way, when it can't.
    pub stuck: Option<String>,
}

/// One chain the service keeps.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ChainHealth {
    pub label: String,
    /// The account's coin, in whole units.
    pub gas: String,
    /// Why the last round on this chain stopped, if it did.
    pub error: Option<String>,
    pub feeds: Vec<FeedHealth>,
    pub pools: Vec<PoolHealth>,
}

/// Everything `/health` answers.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Health {
    pub account: String,
    /// The Solana account, when Solana is kept.
    pub solana_account: Option<String>,
    /// Seconds between rounds.
    pub every: u64,
    pub rounds: u64,
    pub last_round: Option<u64>,
    pub last_clean_round: Option<u64>,
    pub chains: BTreeMap<String, ChainHealth>,
}

impl Health {
    /// A chain's entry, made if it isn't there yet.
    pub fn chain(&mut self, key: &str) -> &mut ChainHealth {
        self.chains.entry(key.to_string()).or_default()
    }

    /// Whether all is well at `now` (unix seconds): a round finished on every
    /// chain within three rounds' time. Before the first round, nothing is
    /// well yet.
    pub fn up(&self, now: u64) -> bool {
        let window = self.every.saturating_mul(3).max(180);
        self.last_clean_round
            .is_some_and(|at| now.saturating_sub(at) <= window)
    }

    /// The first line of the answer: what a dashboard's row shows.
    pub fn headline(&self, now: u64) -> String {
        if self.up(now) {
            let stuck = self
                .chains
                .values()
                .flat_map(|chain| &chain.pools)
                .filter(|pool| pool.stuck.is_some())
                .count();
            if stuck == 0 {
                "testnet-market: every pool kept".to_string()
            } else {
                format!("testnet-market: up, {stuck} pool(s) can't be moved all the way")
            }
        } else if let Some((key, error)) = self
            .chains
            .iter()
            .find_map(|(key, chain)| chain.error.as_ref().map(|error| (key, error)))
        {
            let mut line = format!("testnet-market: {key}: {error}");
            line.truncate(160);
            line
        } else if self.last_round.is_none() {
            "testnet-market: no round yet".to_string()
        } else {
            "testnet-market: no clean round lately".to_string()
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// Serve `/health` on `address`: 200 when up, else 503, the headline then
/// the whole report as JSON.
pub async fn serve(address: SocketAddr, health: Arc<Mutex<Health>>) -> anyhow::Result<()> {
    let app = axum::Router::new().route(
        "/health",
        axum::routing::get(move || {
            let health = health.clone();
            async move {
                let Ok(health) = health.lock().map(|health| health.clone()) else {
                    return (
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "testnet-market: health unreadable".to_string(),
                    );
                };
                let now = now();
                let status = if health.up(now) {
                    axum::http::StatusCode::OK
                } else {
                    axum::http::StatusCode::SERVICE_UNAVAILABLE
                };
                let body = serde_json::to_string_pretty(&health).unwrap_or_default();
                (status, format!("{}\n{body}\n", health.headline(now)))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address, "health on /health");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_up_before_a_clean_round_or_long_after_the_last() {
        let mut health = Health {
            every: 60,
            ..Health::default()
        };
        assert!(!health.up(1_000));
        assert_eq!(health.headline(1_000), "testnet-market: no round yet");
        health.last_round = Some(1_000);
        health.last_clean_round = Some(1_000);
        assert!(health.up(1_100));
        assert!(!health.up(1_000 + 181));
        health.chain("evm:46630").error = Some("the node refused".into());
        assert_eq!(
            health.headline(2_000),
            "testnet-market: evm:46630: the node refused"
        );
    }

    #[test]
    fn a_pool_it_cannot_move_is_counted_in_the_headline() {
        let mut health = Health {
            every: 60,
            last_round: Some(10),
            last_clean_round: Some(10),
            ..Health::default()
        };
        health.chain("evm:84532").pools.push(PoolHealth {
            symbol: "WETH".into(),
            stuck: Some("can't move it up: no USDC above the floor".into()),
            ..PoolHealth::default()
        });
        assert_eq!(
            health.headline(20),
            "testnet-market: up, 1 pool(s) can't be moved all the way"
        );
    }
}
